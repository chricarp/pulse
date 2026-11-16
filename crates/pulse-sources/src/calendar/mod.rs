//! Calendars from a local `.ics` file or an ICS URL: `calendar.event.starting` (`lead` before the
//! start) and `calendar.event.ended`.
//!
//! The calendar is re-read every `refresh` (default 15m; for a URL with `If-None-Match` /
//! `If-Modified-Since`, so an unchanged feed costs a 304). Between reads one wall-clock timer is
//! armed for the next fire or the next read, whichever is first.
//!
//! **Firing rules** ([`Planner`]): each occurrence fires `starting` and `ended` at most once,
//! keyed by its identity (see [`expand`]), so an event edited or moved between reads does not
//! fire again, and one removed or cancelled before its time does not fire at all. A fire whose
//! time passed while the machine slept fires late if it is less than [`LATE_GRACE`] overdue
//! (`minutes_until` then says how late); older ones are dropped, never replayed as a backlog. What
//! fired is kept in a state file in the Pulse home, so a restart does not repeat it.
//!
//! **Untrusted input:** the download is capped at [`MAX_ICS_BYTES`] with a 30 s timeout, the
//! number of events and instances is capped, and titles and locations are cut to 1 KiB.

mod expand;
mod ics;

use std::collections::BTreeMap;
use std::sync::Arc;

use pulse_contracts::catalog::{BuiltinEvent, CalendarEventData};
use pulse_contracts::config::CalendarSourceSettings;
use pulse_contracts::{Duration, SourceKind, SourceRef, Timestamp};
use serde::{Deserialize, Serialize};

use self::expand::{Occurrence, Zone};
use crate::clock::{Clock, SystemClock};
use crate::state::StateFile;
use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

const DEFAULT_REFRESH: Duration = Duration::from_mins(15);
const DEFAULT_LEAD: Duration = Duration::from_mins(10);
/// A fire overdue by less than this (after a suspend or restart) still fires, marked by its
/// `minutes_until`; one overdue by more is dropped.
pub const LATE_GRACE: Duration = Duration::from_mins(15);
pub const MAX_ICS_BYTES: u64 = 10 * 1024 * 1024;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const FIRST_RETRY: Duration = Duration::from_mins(1);
const MAX_FIRED: usize = 50_000;
/// The largest `refresh` and `lead`: a year. More is a typo, and would stretch the expansion
/// window past anything the planner needs.
const MAX_SETTING: Duration = Duration::from_days(365);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Starting,
    Ended,
}

/// Decides fires from the occurrences known now. Pure: occurrences and `now` in, fires and the
/// next deadline out.
#[derive(Debug, Default)]
struct Planner {
    lead: Duration,
    /// (occurrence key, kind) → the occurrence's end, for pruning.
    fired: BTreeMap<(String, Kind), Timestamp>,
}

impl Planner {
    fn due(
        &mut self,
        occs: &[Occurrence],
        now: Timestamp,
    ) -> (Vec<(Kind, Occurrence)>, Option<Timestamp>) {
        let mut out = Vec::new();
        let mut next: Option<Timestamp> = None;
        for o in occs {
            for kind in [Kind::Starting, Kind::Ended] {
                let key = (o.key.clone(), kind);
                if self.fired.contains_key(&key) {
                    continue;
                }
                let (at, anchor) = match kind {
                    Kind::Starting => (o.start.saturating_sub(self.lead), o.start),
                    Kind::Ended => (o.end, o.end),
                };
                if now < at {
                    next = Some(next.map_or(at, |n| n.min(at)));
                    continue;
                }
                if now <= anchor.saturating_add(LATE_GRACE) {
                    out.push((kind, o.clone()));
                }
                // Fired, or too late to matter: either way, done with it.
                self.fired.insert(key, o.end);
            }
        }
        let horizon = now.saturating_sub(Duration::from_days(2));
        self.fired.retain(|_, end| *end >= horizon);
        while self.fired.len() > MAX_FIRED {
            let oldest = self.fired.iter().min_by_key(|(_, end)| **end).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.fired.remove(&k),
                None => break,
            };
        }
        (out, next)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// The calendar's location; state for another one is ignored.
    origin: String,
    fired: Vec<(String, Kind, Timestamp)>,
    etag: Option<String>,
    last_modified: Option<String>,
}

#[derive(Clone, Debug)]
enum Origin {
    File(std::path::PathBuf),
    Url(String),
}

impl Origin {
    fn describe(&self) -> String {
        match self {
            Origin::File(p) => p.display().to_string(),
            Origin::Url(u) => u.clone(),
        }
    }
}

#[derive(Debug)]
pub struct CalendarSource {
    source: SourceRef,
    origin: Origin,
    refresh: Duration,
    lead: Duration,
    zone: Zone,
    state: StateFile,
    clock: Arc<dyn Clock>,
}

impl CalendarSource {
    pub fn new(
        name: &str,
        settings: &CalendarSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        let origin = match (&settings.url, &settings.path) {
            (Some(u), None) if u.starts_with("https://") || u.starts_with("http://") => {
                Origin::Url(u.clone())
            }
            (Some(u), None) => {
                return Err(SourceError::Config(format!("url must be http(s), got {u:?}")));
            }
            (None, Some(p)) => Origin::File(
                pulse_contracts::pattern::expand_path(p, &env.home, None)
                    .map_err(|e| SourceError::Config(format!("path: {e}")))?
                    .into(),
            ),
            _ => return Err(SourceError::Config("set exactly one of `url` and `path`".into())),
        };
        let refresh = settings.refresh.unwrap_or(DEFAULT_REFRESH);
        if refresh.is_zero() {
            return Err(SourceError::Config("refresh must be positive".into()));
        }
        let lead = settings.lead.unwrap_or(DEFAULT_LEAD);
        for (what, d) in [("refresh", refresh), ("lead", lead)] {
            if d > MAX_SETTING {
                return Err(SourceError::Config(format!("{what} must be at most {MAX_SETTING}")));
            }
        }
        let source = SourceRef::new(SourceKind::Calendar, name);
        Ok(CalendarSource {
            state: StateFile::new(env, &source),
            source,
            origin,
            refresh,
            lead,
            zone: Zone::System,
            clock: Arc::new(SystemClock),
        })
    }
}

impl Source for CalendarSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            let result = run(*self, &cx).await;
            match &result {
                Ok(()) => cx.health.stopped(),
                Err(e) => cx.health.failed(e.to_string()),
            }
            result
        })
    }
}

/// The result of reading the calendar.
#[derive(Debug)]
enum Fetched {
    Unchanged,
    Body { text: String, etag: Option<String>, last_modified: Option<String> },
}

#[derive(Debug, Default)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
    /// File: modification time and length of the last read.
    file_stamp: Option<(std::time::SystemTime, u64)>,
}

fn read_file(
    path: &std::path::Path,
    v: &Validators,
) -> Result<(Fetched, Option<(std::time::SystemTime, u64)>), String> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} does not exist", path.display()),
        _ => format!("cannot read {}: {e}", path.display()),
    })?;
    if meta.len() > MAX_ICS_BYTES {
        return Err(format!("{} is larger than {MAX_ICS_BYTES} bytes", path.display()));
    }
    let stamp = meta.modified().ok().map(|m| (m, meta.len()));
    if stamp.is_some() && stamp == v.file_stamp {
        return Ok((Fetched::Unchanged, stamp));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((
        Fetched::Body {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            etag: None,
            last_modified: None,
        },
        stamp,
    ))
}

/// One HTTP GET with conditional headers, a size cap and a timeout. Blocking; run it off the
/// async threads.
fn fetch_url(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Fetched, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(5)
        .user_agent(concat!("pulse/", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent: ureq::Agent = config.into();
    let mut req = agent.get(url);
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    if let Some(m) = last_modified {
        req = req.header("If-Modified-Since", m);
    }
    let mut resp = req.call().map_err(|e| format!("cannot fetch {url}: {e}"))?;
    let status = resp.status().as_u16();
    if status == 304 {
        return Ok(Fetched::Unchanged);
    }
    if !(200..300).contains(&status) {
        return Err(format!("cannot fetch {url}: HTTP {status}"));
    }
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(256).collect::<String>())
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_ICS_BYTES)
        .read_to_string()
        .map_err(|e| format!("cannot read {url}: {e} (the limit is {MAX_ICS_BYTES} bytes)"))?;
    Ok(Fetched::Body { text, etag, last_modified })
}

async fn run(src: CalendarSource, cx: &SourceContext) -> Result<(), SourceError> {
    let origin_key = src.origin.describe();
    let saved = src.state.load::<Saved>().filter(|s| s.origin == origin_key).unwrap_or_default();
    let body_cache = src.state.sibling("ics");
    let mut planner = Planner { lead: src.lead, fired: BTreeMap::new() };
    for (k, kind, end) in saved.fired {
        planner.fired.insert((k, kind), end);
    }
    let mut validators =
        Validators { etag: saved.etag, last_modified: saved.last_modified, file_stamp: None };
    // A URL answered with 304 needs the body from last time.
    let mut calendar = match &src.origin {
        Origin::Url(_) => std::fs::read_to_string(&body_cache).ok().map(|t| ics::parse(&t)),
        Origin::File(_) => None,
    };
    if calendar.is_none() {
        validators.etag = None;
        validators.last_modified = None;
    }
    let mut next_fetch = src.clock.now();
    let mut failures: u32 = 0;
    loop {
        let mut dirty = false;
        let now = src.clock.now();
        if now >= next_fetch {
            let result = match &src.origin {
                Origin::File(p) => read_file(p, &validators).map(|(f, stamp)| {
                    validators.file_stamp = stamp;
                    f
                }),
                Origin::Url(u) => {
                    let (u, e, m) =
                        (u.clone(), validators.etag.clone(), validators.last_modified.clone());
                    let job = tokio::task::spawn_blocking(move || {
                        fetch_url(&u, e.as_deref(), m.as_deref(), FETCH_TIMEOUT)
                    });
                    tokio::select! {
                        () = cx.shutdown.cancelled() => return Ok(()),
                        r = job => r.unwrap_or_else(|e| Err(format!("the fetch task failed: {e}"))),
                    }
                }
            };
            let now = src.clock.now();
            match result {
                Ok(fetched) => {
                    failures = 0;
                    next_fetch = now.saturating_add(src.refresh);
                    if let Fetched::Body { text, etag, last_modified } = fetched {
                        let cal = ics::parse(&text);
                        for w in &cal.warnings {
                            cx.health.error(w.clone());
                        }
                        if matches!(src.origin, Origin::Url(_)) {
                            if let Err(e) = crate::state::write_atomic(&body_cache, text.as_bytes())
                            {
                                cx.health.error(format!("cannot cache the calendar: {e}"));
                            }
                            validators.etag = etag;
                            validators.last_modified = last_modified;
                        }
                        calendar = Some(cal);
                        dirty = true;
                    }
                    cx.health.healthy();
                }
                Err(message) => {
                    failures += 1;
                    let wait = Duration::from_millis(
                        FIRST_RETRY
                            .as_millis()
                            .saturating_mul(1 << (failures - 1).min(16))
                            .min(src.refresh.as_millis()),
                    );
                    next_fetch = now.saturating_add(wait);
                    cx.health.degraded(format!("{message}; retrying in {wait}"));
                }
            }
        }
        let now = src.clock.now();
        let mut next = next_fetch;
        if let Some(cal) = &calendar {
            let horizon = src
                .refresh
                .max(Duration::from_hours(1))
                .as_millis()
                .saturating_add(src.lead.as_millis())
                .saturating_add(Duration::from_days(1).as_millis());
            let to = now.saturating_add(Duration::from_millis(horizon));
            let (occs, warnings) =
                expand::occurrences(cal, src.zone, now.saturating_sub(Duration::from_days(1)), to);
            for w in warnings {
                cx.health.error(w);
            }
            let (fires, next_fire) = planner.due(&occs, now);
            if let Some(n) = next_fire {
                next = next.min(n);
            }
            let fired_any = !fires.is_empty();
            for (kind, o) in fires {
                let data = CalendarEventData {
                    uid: o.uid.clone(),
                    title: o.title.clone(),
                    start: o.start,
                    end: o.end,
                    location: o.location.clone(),
                    // Rounded to the nearest minute, so a fire exactly `lead` early says `lead`.
                    minutes_until: o
                        .start
                        .as_millis()
                        .saturating_sub(now.as_millis())
                        .saturating_add(30_000)
                        .div_euclid(60_000),
                };
                let ev = match kind {
                    Kind::Starting => BuiltinEvent::CalendarEventStarting(data),
                    Kind::Ended => BuiltinEvent::CalendarEventEnded(data),
                };
                let mut ev = ev.into_new_event(src.source.clone(), now);
                ev.dedupe_key = Some(format!("{}:{}:{kind:?}", src.source.uri(), o.key));
                tokio::select! {
                    () = cx.shutdown.cancelled() => return Ok(()),
                    r = cx.events.send(ev) => r?,
                }
            }
            if fired_any || dirty {
                let saved = Saved {
                    origin: origin_key.clone(),
                    fired: planner
                        .fired
                        .iter()
                        .map(|((k, kind), end)| (k.clone(), *kind, *end))
                        .collect(),
                    etag: validators.etag.clone(),
                    last_modified: validators.last_modified.clone(),
                };
                if let Err(e) = src.state.store(&saved) {
                    cx.health.error(format!("cannot save calendar state: {e}"));
                }
            }
        }
        tokio::select! {
            () = cx.shutdown.cancelled() => return Ok(()),
            () = src.clock.sleep_until(next) => {}
        }
    }
}

#[cfg(test)]
mod tests;
