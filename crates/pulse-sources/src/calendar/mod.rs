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
//! **Untrusted input:** the download is capped at [`MAX_ICS_BYTES`] with a 30 s timeout for the
//! whole fetch, a URL may not lead the daemon to local addresses (see [`http`]), the number of
//! events and instances is capped, and titles and locations are cut to 1 KiB. Parsing
//! and expansion run on the blocking pool, and an expansion's cost is bounded whatever the file
//! says (see [`expand`] and [`recur`]): when a budget is reached, health says which event and
//! which limit, and everything within budget still fires.
//!
//! **Work per wake.** A calendar is parsed only when its content changed (a 304, an unchanged
//! file, or a body identical to the last one is not parsed again), and expanded only when it
//! changed or the window it was expanded for (a day past what the next read needs) runs out, so
//! a fire costs one pass of the planner over the occurrences, not a new expansion.

mod expand;
mod http;
mod ics;
mod recur;

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use pulse_contracts::catalog::{BuiltinEvent, CalendarEventData};
use pulse_contracts::config::CalendarSourceSettings;
use pulse_contracts::{Duration, SourceKind, SourceRef, Timestamp};
use serde::{Deserialize, Serialize};

use self::expand::{Expanded, Occurrence, Zone};
use self::http::{UrlPolicy, fetch_url};
use self::ics::Calendar;
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
    /// Entries visited, so tests can bound the work of a call.
    steps: usize,
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
        self.steps += occs.len() * 2 + self.fired.len();
        self.fired.retain(|_, end| *end >= horizon);
        if self.fired.len() > MAX_FIRED {
            // Keep the latest-ending, in one pass.
            let mut by_end: Vec<_> = std::mem::take(&mut self.fired).into_iter().collect();
            self.steps += by_end.len();
            let cut = by_end.len() - MAX_FIRED;
            by_end.select_nth_unstable_by_key(cut, |(_, end)| *end);
            self.fired = by_end.into_iter().skip(cut).collect();
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
    /// What a URL's fetches may reach (see [`http`]).
    policy: UrlPolicy,
    /// Expansions done, so tests can check a fire does not cause one.
    #[cfg(test)]
    expansions: Arc<std::sync::atomic::AtomicUsize>,
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
            policy: UrlPolicy::default(),
            #[cfg(test)]
            expansions: Arc::default(),
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

#[derive(Clone, Debug, Default)]
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
    // Not a pipe or a device: reading one could block or never end.
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let stamp = meta.modified().ok().map(|m| (m, meta.len()));
    if stamp.is_some() && stamp == v.file_stamp {
        return Ok((Fetched::Unchanged, stamp));
    }
    // The size is checked on what is read, not on the metadata: the file may grow meanwhile.
    let bytes = read_limited(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .ok_or_else(|| format!("{} is larger than {MAX_ICS_BYTES} bytes", path.display()))?;
    Ok((
        Fetched::Body {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            etag: None,
            last_modified: None,
        },
        stamp,
    ))
}

/// A file's contents, or `None` if it is larger than [`MAX_ICS_BYTES`].
fn read_limited(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAX_ICS_BYTES + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_ICS_BYTES).then_some(bytes))
}

/// Runs `f` on the blocking pool. `None` if the source is stopped meanwhile (the job finishes
/// on its own, within its budget). A panic there ends the source: it would repeat on the same
/// input.
async fn blocking<T: Send + 'static>(
    cx: &SourceContext,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<Option<T>, SourceError> {
    let job = tokio::task::spawn_blocking(f);
    tokio::select! {
        () = cx.shutdown.cancelled() => Ok(None),
        r = job => r.map(Some).map_err(|e| SourceError::Platform(format!("reading the calendar failed: {e}"))),
    }
}

/// A new version of the calendar, parsed.
struct Got {
    cal: Calendar,
    hash: u64,
}

fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Reads the calendar and, if its content is new, parses it (and caches a URL's body). Updates
/// the validators for the next read. Blocking.
fn read_and_parse(
    origin: &Origin,
    policy: &UrlPolicy,
    v: &mut Validators,
    last_hash: Option<u64>,
    cache: &std::path::Path,
) -> Result<(Option<Got>, Option<String>), String> {
    let fetched = match origin {
        Origin::File(p) => {
            let (f, stamp) = read_file(p, v)?;
            v.file_stamp = stamp;
            f
        }
        Origin::Url(u) => {
            fetch_url(u, v.etag.as_deref(), v.last_modified.as_deref(), FETCH_TIMEOUT, policy)?
        }
    };
    let Fetched::Body { text, etag, last_modified } = fetched else { return Ok((None, None)) };
    if matches!(origin, Origin::Url(_)) {
        v.etag = etag;
        v.last_modified = last_modified;
    }
    let hash = hash_text(&text);
    if Some(hash) == last_hash {
        return Ok((None, None));
    }
    let mut cache_error = None;
    if matches!(origin, Origin::Url(_)) {
        if let Err(e) = crate::state::write_atomic(cache, text.as_bytes()) {
            cache_error = Some(format!("cannot cache the calendar: {e}"));
        }
    }
    Ok((Some(Got { cal: ics::parse(&text), hash }), cache_error))
}

/// The occurrences of one calendar version in a window that reaches `to`.
struct Expansion {
    occs: Vec<Occurrence>,
    to: Timestamp,
}

/// What the source's health shows besides fetch problems: budgets reached.
fn limits_message(limits: &[String]) -> Option<String> {
    let (first, rest) =
        (limits.iter().take(3).cloned().collect::<Vec<_>>(), limits.len().saturating_sub(3));
    if first.is_empty() {
        return None;
    }
    let mut m = format!("some occurrences are not read: {}", first.join("; "));
    if rest > 0 {
        m.push_str(&format!(" (and {rest} more)"));
    }
    Some(m)
}

async fn run(src: CalendarSource, cx: &SourceContext) -> Result<(), SourceError> {
    let origin_key = src.origin.describe();
    let saved = src.state.load::<Saved>().filter(|s| s.origin == origin_key).unwrap_or_default();
    let body_cache = src.state.sibling("ics");
    let mut planner = Planner { lead: src.lead, fired: BTreeMap::new(), steps: 0 };
    for (k, kind, end) in saved.fired {
        planner.fired.insert((k, kind), end);
    }
    let mut validators =
        Validators { etag: saved.etag, last_modified: saved.last_modified, file_stamp: None };
    // A URL answered with 304 needs the body from last time.
    let mut last_hash: Option<u64> = None;
    let mut calendar: Option<Arc<Calendar>> = match &src.origin {
        Origin::Url(_) => {
            let cache = body_cache.clone();
            let job = blocking(cx, move || {
                let bytes = read_limited(&cache).ok().flatten()?;
                let text = String::from_utf8_lossy(&bytes);
                Some((ics::parse(&text), hash_text(&text)))
            });
            let Some(cached) = job.await? else { return Ok(()) };
            cached.map(|(cal, hash)| {
                last_hash = Some(hash);
                Arc::new(cal)
            })
        }
        Origin::File(_) => None,
    };
    if calendar.is_none() {
        validators.etag = None;
        validators.last_modified = None;
    }
    let mut expansion: Option<Expansion> = None;
    let mut fetch_problem: Option<String> = None;
    let mut limit_problem: Option<String> = None;
    let mut shown: Option<Option<String>> = None;
    let mut next_fetch = src.clock.now();
    let mut failures: u32 = 0;
    // What the planner needs ahead of now: up to the next read, plus the lead.
    let ahead = Duration::from_millis(
        src.refresh.max(Duration::from_hours(1)).as_millis().saturating_add(src.lead.as_millis()),
    );
    loop {
        let mut dirty = false;
        let now = src.clock.now();
        if now >= next_fetch {
            let (origin, mut v, cache) =
                (src.origin.clone(), validators.clone(), body_cache.clone());
            let policy = src.policy.clone();
            let job = blocking(cx, move || {
                let r = read_and_parse(&origin, &policy, &mut v, last_hash, &cache);
                (r, v)
            });
            let Some((result, v)) = job.await? else { return Ok(()) };
            dirty = v.etag != validators.etag || v.last_modified != validators.last_modified;
            validators = v;
            let now = src.clock.now();
            match result {
                Ok((got, cache_error)) => {
                    failures = 0;
                    fetch_problem = None;
                    next_fetch = now.saturating_add(src.refresh);
                    if let Some(e) = cache_error {
                        cx.health.error(e);
                    }
                    if let Some(got) = got {
                        for w in &got.cal.warnings {
                            cx.health.error(w.clone());
                        }
                        last_hash = Some(got.hash);
                        calendar = Some(Arc::new(got.cal));
                        expansion = None;
                        dirty = true;
                    }
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
                    fetch_problem = Some(format!("{message}; retrying in {wait}"));
                }
            }
        }
        let now = src.clock.now();
        let mut next = next_fetch;
        if let Some(cal) = &calendar {
            let needed = now.saturating_add(ahead);
            if expansion.as_ref().is_none_or(|e| e.to < needed) {
                let (cal, zone) = (cal.clone(), src.zone);
                let (from, to) = (
                    now.saturating_sub(Duration::from_days(1)),
                    needed.saturating_add(Duration::from_days(1)),
                );
                let Some(x): Option<Expanded> =
                    blocking(cx, move || expand::occurrences(&cal, zone, from, to)).await?
                else {
                    return Ok(());
                };
                #[cfg(test)]
                src.expansions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                for w in x.warnings {
                    cx.health.error(w);
                }
                limit_problem = limits_message(&x.limits);
                expansion = Some(Expansion { occs: x.occs, to });
            }
        }
        if let Some(exp) = &expansion {
            let (fires, next_fire) = planner.due(&exp.occs, now);
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
        // Only on a change: a fetch failure first, then budgets reached, else healthy.
        let problem = fetch_problem.clone().or_else(|| limit_problem.clone());
        if shown.as_ref() != Some(&problem) {
            match &problem {
                Some(m) => cx.health.degraded(m.clone()),
                None => cx.health.healthy(),
            }
            shown = Some(problem);
        }
        tokio::select! {
            () = cx.shutdown.cancelled() => return Ok(()),
            () = src.clock.sleep_until(next) => {}
        }
    }
}

#[cfg(test)]
mod tests;
