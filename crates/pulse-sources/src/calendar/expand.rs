//! From parsed `VEVENT`s to concrete occurrences in a time window.
//!
//! - Times: `…Z` is UTC; `TZID=…` is that zone (IANA names, IANA names behind a vendor prefix
//!   such as `/mozilla.org/…/Europe/Rome`, common Windows names as Outlook writes them, and
//!   failing those the zone's standard offset from its `VTIMEZONE`); a time with neither is
//!   floating and means the calendar's `X-WR-TIMEZONE` or else the user's zone; a `DATE` (all-day
//!   event) is midnight in the user's zone.
//! - Recurrence: `RRULE`, `RDATE` and `EXDATE` through the `rrule` crate. An instance that a
//!   `RECURRENCE-ID` override replaces is dropped and the override used instead.
//! - `STATUS:CANCELLED` removes the event (on a master) or the instance (on an override).
//! - Identity: a one-off event is its `UID`; a recurring instance is `UID` plus its original
//!   start (the `RECURRENCE-ID`). Moving an event keeps its identity, so a moved event does not
//!   fire twice.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use pulse_contracts::Timestamp;
use rrule::{RRule, RRuleSet, Tz, Unvalidated};

use super::ics::{Calendar, Prop, VEvent};

/// The user's zone, for floating times and all-day events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zone {
    System,
    /// A fixed zone. Only tests construct it (to not depend on the machine's zone); the source
    /// always uses the system zone.
    #[cfg_attr(not(test), allow(dead_code))]
    Named(chrono_tz::Tz),
}

impl Zone {
    fn tz(self) -> Tz {
        match self {
            Zone::System => Tz::LOCAL,
            Zone::Named(t) => Tz::Tz(t),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Occurrence {
    pub key: String,
    pub uid: String,
    pub title: String,
    pub location: Option<String>,
    pub start: Timestamp,
    pub end: Timestamp,
}

const MAX_INSTANCES_PER_EVENT: u16 = 2_000;

/// A time value with how to read it.
#[derive(Clone, Copy, Debug)]
enum When {
    Utc(NaiveDateTime),
    In(NaiveDateTime, Tz),
    Date(NaiveDate),
}

struct Ctx<'a> {
    cal: &'a Calendar,
    local: Zone,
    warnings: BTreeSet<String>,
}

impl Ctx<'_> {
    fn zone(&mut self, tzid: &str) -> Tz {
        if let Some(t) = named_zone(tzid) {
            return Tz::Tz(t);
        }
        if let Some(t) = self.cal.tz_offsets.get(tzid).and_then(|o| offset_zone(o)) {
            return Tz::Tz(t);
        }
        self.warnings
            .insert(format!("unknown time zone {:?}; its times are read as UTC", bounded(tzid)));
        Tz::UTC
    }

    fn floating(&mut self) -> Tz {
        match self.cal.default_tz.as_deref().and_then(named_zone) {
            Some(t) => Tz::Tz(t),
            None => self.local.tz(),
        }
    }

    fn values(&mut self, prop: &Prop) -> Vec<When> {
        let date_only = prop.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        let tz = prop.param("TZID").map(str::to_owned);
        prop.value
            .split(',')
            .filter_map(|v| {
                let v = v.trim();
                if date_only || v.len() == 8 {
                    return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(When::Date);
                }
                let (body, utc) = match v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
                    Some(b) => (b, true),
                    None => (v, false),
                };
                let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
                Some(match (&tz, utc) {
                    (_, true) => When::Utc(naive),
                    (Some(id), false) => When::In(naive, self.zone(id)),
                    (None, false) => When::In(naive, self.floating()),
                })
            })
            .collect()
    }

    fn value(&mut self, prop: &Prop) -> Option<When> {
        self.values(prop).into_iter().next()
    }

    fn instant(&self, w: When) -> Option<DateTime<Tz>> {
        match w {
            When::Utc(n) => Some(Utc.from_utc_datetime(&n).with_timezone(&Tz::UTC)),
            When::In(n, tz) => local(tz, n),
            When::Date(d) => local(self.local.tz(), d.and_hms_opt(0, 0, 0)?),
        }
    }
}

/// A local time in `tz`: the earlier of two, or just after a gap.
fn local(tz: Tz, n: NaiveDateTime) -> Option<DateTime<Tz>> {
    let mut probe = n;
    for _ in 0..(24 * 60) {
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => return Some(dt),
            LocalResult::None => probe += Duration::minutes(1),
        }
    }
    None
}

fn bounded(s: &str) -> String {
    s.chars().take(64).collect()
}

/// IANA names, possibly behind a vendor prefix, and the Windows names Outlook uses.
fn named_zone(id: &str) -> Option<chrono_tz::Tz> {
    let id = id.trim();
    if let Ok(t) = chrono_tz::Tz::from_str(id) {
        return Some(t);
    }
    // `/mozilla.org/20050126_1/Europe/Rome`, `/softwarestudio.org/Olson_20011030_5/Europe/Rome`
    for (i, c) in id.char_indices() {
        if c == '/' {
            if let Ok(t) = chrono_tz::Tz::from_str(&id[i + 1..]) {
                return Some(t);
            }
        }
    }
    const WINDOWS: &[(&str, &str)] = &[
        ("UTC", "UTC"),
        ("GMT Standard Time", "Europe/London"),
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Romance Standard Time", "Europe/Paris"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("Central European Standard Time", "Europe/Warsaw"),
        ("E. Europe Standard Time", "Europe/Chisinau"),
        ("FLE Standard Time", "Europe/Kyiv"),
        ("GTB Standard Time", "Europe/Bucharest"),
        ("Russian Standard Time", "Europe/Moscow"),
        ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"),
        ("Mountain Standard Time", "America/Denver"),
        ("US Mountain Standard Time", "America/Phoenix"),
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("Alaskan Standard Time", "America/Anchorage"),
        ("Hawaiian Standard Time", "Pacific/Honolulu"),
        ("Atlantic Standard Time", "America/Halifax"),
        ("E. South America Standard Time", "America/Sao_Paulo"),
        ("India Standard Time", "Asia/Kolkata"),
        ("China Standard Time", "Asia/Shanghai"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("Korea Standard Time", "Asia/Seoul"),
        ("Singapore Standard Time", "Asia/Singapore"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
        ("New Zealand Standard Time", "Pacific/Auckland"),
        ("Israel Standard Time", "Asia/Jerusalem"),
        ("Arabian Standard Time", "Asia/Dubai"),
        ("South Africa Standard Time", "Africa/Johannesburg"),
    ];
    WINDOWS.iter().find(|(w, _)| *w == id).and_then(|(_, iana)| chrono_tz::Tz::from_str(iana).ok())
}

/// `+0100` → `Etc/GMT-1` (the sign is inverted in that namespace). Whole hours only.
fn offset_zone(offset: &str) -> Option<chrono_tz::Tz> {
    let (sign, rest) = match offset.as_bytes().first()? {
        b'+' => (-1, &offset[1..]),
        b'-' => (1, &offset[1..]),
        _ => return None,
    };
    let hours: i32 = rest.get(..2)?.parse().ok()?;
    if rest.get(2..4).is_some_and(|m| m != "00") {
        return None;
    }
    let name = if hours == 0 { "Etc/GMT".to_owned() } else { format!("Etc/GMT{:+}", sign * hours) };
    chrono_tz::Tz::from_str(&name).ok()
}

/// RFC 5545 durations: `P1D`, `PT1H30M`, `P2W`, `-PT15M`.
pub(crate) fn parse_duration(s: &str) -> Option<Duration> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut time = false;
    for c in s.chars() {
        match c {
            'T' => time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (c, time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(if neg { -total } else { total })
}

fn ts(dt: &DateTime<Tz>) -> Timestamp {
    Timestamp::from_millis(dt.timestamp_millis())
}

fn chrono(t: Timestamp) -> Option<DateTime<Tz>> {
    DateTime::from_timestamp_millis(t.as_millis()).map(|d| d.with_timezone(&Tz::UTC))
}

/// Rewrites `UNTIL` as UTC, read in the event's zone: the `rrule` crate requires a UTC `UNTIL`
/// for zoned starts, and exports write it as a date, a local time, or UTC. A date means the
/// end of that day.
fn normalize_until(rule: &str, ctx: &mut Ctx<'_>, start_zone: Tz) -> String {
    rule.split(';')
        .map(|part| {
            let Some(v) = part.strip_prefix("UNTIL=").or_else(|| part.strip_prefix("until="))
            else {
                return part.to_owned();
            };
            let when = if v.ends_with('Z') || v.ends_with('z') {
                return part.to_owned();
            } else if v.len() == 8 {
                NaiveDate::parse_from_str(v, "%Y%m%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(23, 59, 59))
                    .map(|n| When::In(n, start_zone))
            } else {
                NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
                    .ok()
                    .map(|n| When::In(n, start_zone))
            };
            match when.and_then(|w| ctx.instant(w)) {
                Some(dt) => format!("UNTIL={}", dt.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")),
                None => part.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn cancelled(ev: &VEvent) -> bool {
    ev.status.as_deref() == Some("CANCELLED")
}

/// Occurrences overlapping `[from, to]`, and warnings about what could not be read.
pub(crate) fn occurrences(
    cal: &Calendar,
    local_zone: Zone,
    from: Timestamp,
    to: Timestamp,
) -> (Vec<Occurrence>, Vec<String>) {
    let mut ctx = Ctx { cal, local: local_zone, warnings: BTreeSet::new() };
    let (Some(from_dt), Some(to_dt)) = (chrono(from), chrono(to)) else {
        return (Vec::new(), Vec::new());
    };
    // Overrides by (UID, original start).
    let mut overrides: BTreeMap<(String, Timestamp), &VEvent> = BTreeMap::new();
    for ev in &cal.events {
        if let (Some(uid), Some(rid)) = (&ev.uid, &ev.recurrence_id) {
            if let Some(orig) = ctx.value(rid).and_then(|w| ctx.instant(w)) {
                overrides.insert((uid.clone(), ts(&orig)), ev);
            }
        }
    }
    let mut out = Vec::new();
    let mut masters: BTreeMap<&str, &VEvent> = BTreeMap::new();
    for (i, ev) in cal.events.iter().enumerate() {
        if ev.recurrence_id.is_some() || cancelled(ev) {
            continue;
        }
        let Some(start_when) = ev.dtstart.as_ref().and_then(|p| ctx.value(p)) else { continue };
        let Some(start) = ctx.instant(start_when) else { continue };
        let duration = span(ev, &mut ctx, start_when, &start);
        // Events without a UID get one from their position; they cannot be overridden anyway.
        let uid = ev.uid.clone().unwrap_or_else(|| format!("no-uid-{i}"));
        if let Some(u) = &ev.uid {
            masters.insert(u, ev);
        }
        let title = ev.summary.clone().unwrap_or_default();
        if ev.rrules.is_empty() && ev.rdates.is_empty() {
            push(&mut out, &uid, uid.clone(), &title, ev, ts(&start), duration, from, to);
            continue;
        }
        let tz = start.timezone();
        let mut set = RRuleSet::new(start);
        for rule in &ev.rrules {
            let rule = normalize_until(rule, &mut ctx, tz);
            match rule.parse::<RRule<Unvalidated>>().and_then(|r| r.validate(start)) {
                Ok(r) => set = set.rrule(r),
                Err(e) => {
                    ctx.warnings.insert(format!(
                        "event {:?}: cannot read RRULE ({e}); using its first occurrence only",
                        bounded(&title)
                    ));
                }
            }
        }
        for p in &ev.rdates {
            for w in ctx.values(p) {
                if let Some(d) = ctx.instant(w) {
                    set = set.rdate(d.with_timezone(&tz));
                }
            }
        }
        for p in &ev.exdates {
            for w in ctx.values(p) {
                if let Some(d) = ctx.instant(w) {
                    set = set.exdate(d.with_timezone(&tz));
                }
            }
        }
        let window_start = from_dt - duration - Duration::seconds(1);
        let instances = set
            .after(window_start.with_timezone(&tz))
            .before(to_dt.with_timezone(&tz))
            .all(MAX_INSTANCES_PER_EVENT);
        for inst in instances.dates {
            let orig = ts(&inst);
            if overrides.contains_key(&(uid.clone(), orig)) {
                continue;
            }
            push(
                &mut out,
                &uid,
                format!("{uid}@{}", orig.as_millis()),
                &title,
                ev,
                orig,
                duration,
                from,
                to,
            );
        }
    }
    for ((uid, orig), ev) in &overrides {
        let Some(master) = masters.get(uid.as_str()) else { continue };
        if cancelled(ev) {
            continue;
        }
        let Some(start_when) = ev.dtstart.as_ref().and_then(|p| ctx.value(p)) else { continue };
        let Some(start) = ctx.instant(start_when) else { continue };
        let duration = if ev.dtend.is_some() || ev.duration.is_some() {
            span(ev, &mut ctx, start_when, &start)
        } else {
            master
                .dtstart
                .as_ref()
                .and_then(|p| ctx.value(p))
                .and_then(|w| ctx.instant(w).map(|s| span(master, &mut ctx, w, &s)))
                .unwrap_or_else(Duration::zero)
        };
        let title = ev.summary.clone().or_else(|| master.summary.clone()).unwrap_or_default();
        let source = if ev.location.is_some() { *ev } else { *master };
        push(
            &mut out,
            uid,
            format!("{uid}@{}", orig.as_millis()),
            &title,
            source,
            ts(&start),
            duration,
            from,
            to,
        );
    }
    out.sort_by(|a, b| (a.start, &a.key).cmp(&(b.start, &b.key)));
    (out, ctx.warnings.into_iter().collect())
}

fn span(ev: &VEvent, ctx: &mut Ctx<'_>, start_when: When, start: &DateTime<Tz>) -> Duration {
    if let Some(end) = ev.dtend.as_ref().and_then(|p| ctx.value(p)).and_then(|w| ctx.instant(w)) {
        return (end.with_timezone(&Utc) - start.with_timezone(&Utc)).max(Duration::zero());
    }
    if let Some(d) = ev.duration.as_deref().and_then(parse_duration) {
        return d.max(Duration::zero());
    }
    // RFC 5545: an all-day event without an end lasts the day; a timed one is an instant.
    if matches!(start_when, When::Date(_)) { Duration::days(1) } else { Duration::zero() }
}

#[allow(clippy::too_many_arguments)]
fn push(
    out: &mut Vec<Occurrence>,
    uid: &str,
    key: String,
    title: &str,
    ev: &VEvent,
    start: Timestamp,
    duration: Duration,
    from: Timestamp,
    to: Timestamp,
) {
    let end = Timestamp::from_millis(start.as_millis().saturating_add(duration.num_milliseconds()));
    if end < from || start > to {
        return;
    }
    out.push(Occurrence {
        key,
        uid: uid.to_owned(),
        title: title.to_owned(),
        location: ev.location.clone(),
        start,
        end,
    });
}

#[cfg(test)]
mod tests;
