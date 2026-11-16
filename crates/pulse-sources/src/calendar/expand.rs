//! From parsed `VEVENT`s to concrete occurrences in a time window, within a budget.
//!
//! - Times: `…Z` is UTC; `TZID=…` is that zone (IANA names, IANA names behind a vendor prefix
//!   such as `/mozilla.org/…/Europe/Rome`, common Windows names as Outlook writes them, and
//!   failing those the zone's standard offset from its `VTIMEZONE`); a time with neither is
//!   floating and means the calendar's `X-WR-TIMEZONE` or else the user's zone; a `DATE` (all-day
//!   event) is midnight in the user's zone.
//! - Recurrence: `RRULE` (see [`super::recur`]), `RDATE` and `EXDATE`. An instance that a
//!   `RECURRENCE-ID` override replaces is dropped and the override used instead. An instance at
//!   a local time skipped by a daylight-saving change is moved forward by the gap (RFC 5545); a
//!   repeated local time is its first occurrence.
//! - `STATUS:CANCELLED` removes the event (on a master) or the instance (on an override).
//! - Identity: a one-off event is its `UID`; a recurring instance is `UID` plus its original
//!   start (the `RECURRENCE-ID`). Moving an event keeps its identity, so a moved event does not
//!   fire twice.
//!
//! **Budget.** Whatever the calendar says, one expansion takes at most [`MAX_STEPS`] steps and
//! yields at most [`MAX_INSTANCES`] occurrences; one recurring event at most
//! [`MAX_EVENT_STEPS`] steps and [`MAX_INSTANCES_PER_EVENT`] occurrences. One-off events and
//! overrides are taken first, so a costly recurring event cannot crowd them out. What a budget
//! cut is listed in [`Expanded::limits`], naming the event and the limit.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use pulse_contracts::Timestamp;

use super::ics::{Calendar, Prop, VEvent};
use super::recur::{self, Steps, Until, Visit};

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
            Zone::System => Tz::Local,
            Zone::Named(t) => Tz::Named(t),
        }
    }
}

/// A zone a time is read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tz {
    Local,
    Utc,
    Named(chrono_tz::Tz),
}

impl Tz {
    fn resolve(self, n: &NaiveDateTime) -> LocalResult<DateTime<Utc>> {
        match self {
            Tz::Local => chrono::Local.from_local_datetime(n).map(|d| d.with_timezone(&Utc)),
            Tz::Utc => LocalResult::Single(Utc.from_utc_datetime(n)),
            Tz::Named(t) => t.from_local_datetime(n).map(|d| d.with_timezone(&Utc)),
        }
    }

    fn to_local(self, t: &DateTime<Utc>) -> NaiveDateTime {
        match self {
            Tz::Local => t.with_timezone(&chrono::Local).naive_local(),
            Tz::Utc => t.naive_utc(),
            Tz::Named(z) => t.with_timezone(&z).naive_local(),
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

pub(crate) const MAX_INSTANCES_PER_EVENT: usize = 2_000;
/// Occurrences of the whole calendar in one window.
pub(crate) const MAX_INSTANCES: usize = 20_000;
/// Steps for one recurring event: a period, a day or a candidate time considered, a date read.
pub(crate) const MAX_EVENT_STEPS: u64 = 100_000;
/// Steps for the whole calendar in one expansion.
pub(crate) const MAX_STEPS: u64 = 2_000_000;
/// The longest an event can last. Longer spans (a typo, or a hostile `DURATION:P99999999W`)
/// are cut to this, with a warning.
pub(crate) const MAX_SPAN: Duration = Duration::days(366);
/// Zone names longer than this are not looked up (real ones are under 64 bytes, vendor prefix
/// included).
const MAX_TZID: usize = 256;
const MAX_CACHED_ZONES: usize = 1_000;

/// The result of one expansion.
#[derive(Debug, Default)]
pub(crate) struct Expanded {
    /// Sorted by start.
    pub occs: Vec<Occurrence>,
    /// What could not be read (unknown zones, unreadable rules, spans cut).
    pub warnings: Vec<String>,
    /// Budgets that were reached, each naming what it cut: some occurrences are missing.
    pub limits: Vec<String>,
    /// Steps taken, so tests can check the budget.
    #[cfg_attr(not(test), allow(dead_code))]
    pub steps: u64,
}

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
    zones: BTreeMap<String, Tz>,
}

impl Ctx<'_> {
    fn zone(&mut self, tzid: &str) -> Tz {
        if let Some(t) = self.zones.get(tzid) {
            return *t;
        }
        let tz = if let Some(t) = named_zone(tzid) {
            Tz::Named(t)
        } else if let Some(t) = self.cal.tz_offsets.get(tzid).and_then(|o| offset_zone(o)) {
            Tz::Named(t)
        } else {
            self.warnings.insert(format!(
                "unknown time zone {:?}; its times are read as UTC",
                bounded(tzid)
            ));
            Tz::Utc
        };
        if self.zones.len() < MAX_CACHED_ZONES {
            self.zones.insert(tzid.to_owned(), tz);
        }
        tz
    }

    fn floating(&mut self) -> Tz {
        match self.cal.default_tz.as_deref().and_then(named_zone) {
            Some(t) => Tz::Named(t),
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
                let (body, utc) = match v.strip_suffix(['Z', 'z']) {
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

    /// The wall time and zone a value is read in.
    fn wall(&self, w: When) -> (NaiveDateTime, Tz) {
        match w {
            When::Utc(n) => (n, Tz::Utc),
            When::In(n, tz) => (n, tz),
            When::Date(d) => (d.and_time(NaiveTime::MIN), self.local.tz()),
        }
    }

    fn instant(&self, w: When) -> Option<DateTime<Utc>> {
        let (n, tz) = self.wall(w);
        local(tz, n)
    }
}

/// A local time in `tz`: the earlier of two, or just after a gap. How `DTSTART` is read.
fn local(tz: Tz, n: NaiveDateTime) -> Option<DateTime<Utc>> {
    let mut probe = n;
    for _ in 0..(24 * 60) {
        match tz.resolve(&probe) {
            LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => return Some(dt),
            LocalResult::None => probe = probe.checked_add_signed(Duration::minutes(1))?,
        }
    }
    None
}

/// A recurrence instance's local time in `tz`, as RFC 5545 reads one: the earlier of two, or,
/// in a gap, as far past midnight as the wall time says (02:30 on a 02:00→03:00 day is 03:30).
fn instance(tz: Tz, n: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.resolve(&n) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Some(dt),
        LocalResult::None => local(tz, n.date().and_time(NaiveTime::MIN))?
            .checked_add_signed(n.time().signed_duration_since(NaiveTime::MIN)),
    }
}

fn bounded(s: &str) -> String {
    s.chars().take(64).collect()
}

/// IANA names, possibly behind a vendor prefix, and the Windows names Outlook uses.
fn named_zone(id: &str) -> Option<chrono_tz::Tz> {
    let id = id.trim();
    if id.len() > MAX_TZID {
        return None;
    }
    if let Ok(t) = chrono_tz::Tz::from_str(id) {
        return Some(t);
    }
    // `/mozilla.org/20050126_1/Europe/Rome`, `/softwarestudio.org/Olson_20011030_5/Europe/Rome`:
    // an IANA name is the text after one of the last few slashes (it has at most two itself).
    let slashes: Vec<usize> = id.match_indices('/').map(|(i, _)| i).collect();
    for &i in slashes.iter().rev().take(4).rev() {
        if let Ok(t) = chrono_tz::Tz::from_str(&id[i + 1..]) {
            return Some(t);
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

/// RFC 5545 durations: `P1D`, `PT1H30M`, `P2W`, `-PT15M`. A span too large to represent
/// saturates at `Duration::MAX` (callers clamp it to [`MAX_SPAN`]).
pub(crate) fn parse_duration(s: &str) -> Option<Duration> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    // Whole seconds, saturating; far beyond any real event either way.
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut time = false;
    for c in s.chars() {
        match c {
            'T' => time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let unit: i64 = match (c, time) {
                    ('W', false) => 7 * 86_400,
                    ('D', false) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.saturating_add(n.saturating_mul(unit));
            }
        }
    }
    if !num.is_empty() {
        return None;
    }
    let d = Duration::try_seconds(total).unwrap_or(Duration::MAX);
    Some(if neg { -d } else { d })
}

fn ts(dt: &DateTime<Utc>) -> Timestamp {
    Timestamp::from_millis(dt.timestamp_millis())
}

fn cancelled(ev: &VEvent) -> bool {
    ev.status.as_deref() == Some("CANCELLED")
}

fn title_of(ev: &VEvent) -> String {
    ev.summary.clone().unwrap_or_default()
}

fn recurring(ev: &VEvent) -> bool {
    !ev.rrules.is_empty() || !ev.rdates.is_empty()
}

/// Occurrences overlapping `[from, to]`, warnings about what could not be read, and the budgets
/// that were reached.
pub(crate) fn occurrences(
    cal: &Calendar,
    local_zone: Zone,
    from: Timestamp,
    to: Timestamp,
) -> Expanded {
    let ctx = Ctx { cal, local: local_zone, warnings: BTreeSet::new(), zones: BTreeMap::new() };
    let mut x = Expander {
        ctx,
        steps: Steps::new(MAX_STEPS),
        out: Vec::new(),
        limits: Vec::new(),
        from,
        to,
        full: false,
        skipped: 0,
    };
    // Overrides by (UID, original start), and masters by UID.
    let mut overrides: BTreeMap<(String, Timestamp), &VEvent> = BTreeMap::new();
    let mut masters: BTreeMap<&str, &VEvent> = BTreeMap::new();
    for ev in &cal.events {
        if x.steps.take(1).is_err() {
            break;
        }
        match (&ev.uid, &ev.recurrence_id) {
            (Some(uid), Some(rid)) => {
                if let Some(orig) = x.ctx.value(rid).and_then(|w| x.ctx.instant(w)) {
                    overrides.insert((uid.clone(), ts(&orig)), ev);
                }
            }
            (Some(uid), None) if !cancelled(ev) => {
                masters.insert(uid, ev);
            }
            _ => {}
        }
    }
    // Cheap ones first: one-off events and overrides.
    for (i, ev) in cal.events.iter().enumerate() {
        if ev.recurrence_id.is_none() && !cancelled(ev) && !recurring(ev) && x.admit() {
            x.one_off(i, ev);
        }
    }
    for ((uid, orig), ev) in &overrides {
        if let Some(master) = masters.get(uid.as_str()) {
            if !cancelled(ev) && x.admit() {
                x.override_instance(uid, *orig, ev, master);
            }
        }
    }
    for (i, ev) in cal.events.iter().enumerate() {
        if ev.recurrence_id.is_none() && !cancelled(ev) && recurring(ev) && x.admit() {
            x.recurring(i, ev, &overrides);
        }
    }
    if x.skipped > 0 {
        x.limits.push(format!(
            "the calendar needs more than {MAX_STEPS} steps to expand; {} events were not read",
            x.skipped
        ));
    }
    let mut occs = x.out;
    occs.sort_by(|a, b| (a.start, &a.key).cmp(&(b.start, &b.key)));
    Expanded {
        occs,
        warnings: x.ctx.warnings.into_iter().collect(),
        limits: x.limits,
        steps: x.steps.used,
    }
}

struct Expander<'a> {
    ctx: Ctx<'a>,
    steps: Steps,
    out: Vec<Occurrence>,
    limits: Vec<String>,
    from: Timestamp,
    to: Timestamp,
    /// The calendar's occurrence budget is spent.
    full: bool,
    /// Events not read because the calendar's step budget is spent.
    skipped: usize,
}

impl Expander<'_> {
    /// Whether there is budget left for one more event.
    fn admit(&mut self) -> bool {
        if self.full {
            return false;
        }
        if self.steps.take(1).is_err() {
            self.skipped += 1;
            return false;
        }
        true
    }

    fn one_off(&mut self, i: usize, ev: &VEvent) {
        let Some(start_when) = ev.dtstart.as_ref().and_then(|p| self.ctx.value(p)) else { return };
        let Some(start) = self.ctx.instant(start_when) else { return };
        let title = title_of(ev);
        let duration = span(ev, &mut self.ctx, start_when, &start, &title);
        // Events without a UID get one from their position; they cannot be overridden anyway.
        let uid = ev.uid.clone().unwrap_or_else(|| format!("no-uid-{i}"));
        self.push(&uid, uid.clone(), &title, ev, ts(&start), duration);
    }

    fn override_instance(&mut self, uid: &str, orig: Timestamp, ev: &VEvent, master: &VEvent) {
        let ctx = &mut self.ctx;
        let Some(start_when) = ev.dtstart.as_ref().and_then(|p| ctx.value(p)) else { return };
        let Some(start) = ctx.instant(start_when) else { return };
        let duration = if ev.dtend.is_some() || ev.duration.is_some() {
            span(ev, ctx, start_when, &start, &title_of(ev))
        } else {
            master
                .dtstart
                .as_ref()
                .and_then(|p| ctx.value(p))
                .and_then(|w| ctx.instant(w).map(|s| span(master, ctx, w, &s, &title_of(master))))
                .unwrap_or_else(Duration::zero)
        };
        let title = ev.summary.clone().or_else(|| master.summary.clone()).unwrap_or_default();
        let source = if ev.location.is_some() { ev } else { master };
        let key = format!("{uid}@{}", orig.as_millis());
        self.push(uid, key, &title, source, ts(&start), duration);
    }

    fn recurring(
        &mut self,
        i: usize,
        ev: &VEvent,
        overrides: &BTreeMap<(String, Timestamp), &VEvent>,
    ) {
        let ctx = &mut self.ctx;
        let Some(start_when) = ev.dtstart.as_ref().and_then(|p| ctx.value(p)) else { return };
        let Some(start) = ctx.instant(start_when) else { return };
        let title = title_of(ev);
        let duration = span(ev, ctx, start_when, &start, &title);
        let uid = ev.uid.clone().unwrap_or_else(|| format!("no-uid-{i}"));
        let mut rules = Vec::new();
        for text in &ev.rrules {
            match recur::parse(text) {
                Ok(r) => rules.push(r),
                Err(e) => {
                    ctx.warnings.insert(format!(
                        "event {:?}: cannot read RRULE ({e}); using its first occurrence only",
                        bounded(&title)
                    ));
                }
            }
        }
        if rules.is_empty() && ev.rdates.is_empty() {
            self.push(&uid, uid.clone(), &title, ev, ts(&start), duration);
            return;
        }
        let event_budget = MAX_EVENT_STEPS.min(self.steps.left());
        let mut steps = Steps::new(event_budget);
        let lo = Timestamp::from_millis(
            self.from.as_millis().saturating_sub(duration.num_milliseconds()),
        );
        let hi = self.to;
        let (wall, tz) = ctx.wall(start_when);
        // Wall times around the window, widened by more than any offset change in use (2 h, at
        // Troll station), so an occurrence an offset change moves into the window is still seen.
        let window = match (chrono_of(lo), chrono_of(hi)) {
            (Some(l), Some(h)) => {
                let margin = Duration::hours(3);
                let first = tz.to_local(&l).checked_sub_signed(margin);
                let last = tz.to_local(&h).checked_add_signed(margin);
                first.zip(last)
            }
            _ => None,
        };
        let (lo_ms, hi_ms, start_ms) = (lo.as_millis(), hi.as_millis(), start.timestamp_millis());
        let mut excluded = BTreeSet::new();
        let mut out_of_steps = false;
        'exdates: for p in &ev.exdates {
            for w in ctx.values(p) {
                if steps.take(1).is_err() {
                    out_of_steps = true;
                    break 'exdates;
                }
                if let Some(d) = ctx.instant(w) {
                    excluded.insert(d.timestamp_millis());
                }
            }
        }
        let mut starts: BTreeSet<i64> = BTreeSet::new();
        let mut capped = false;
        let mut admit = |at: i64, starts: &mut BTreeSet<i64>| -> Visit {
            if at < lo_ms || excluded.contains(&at) {
                return Visit::Continue;
            }
            starts.insert(at);
            if starts.len() > MAX_INSTANCES_PER_EVENT {
                capped = true;
                if starts.pop_last() == Some(at) {
                    return Visit::Stop;
                }
            }
            Visit::Continue
        };
        if let Some(window) = window.filter(|_| !out_of_steps) {
            for rule in &rules {
                let until = rule.until.and_then(|u| match u {
                    Until::Utc(n) => Some(Utc.from_utc_datetime(&n)),
                    Until::Local(n) => instance(tz, n),
                    Until::Date(d) => d.and_hms_opt(23, 59, 59).and_then(|n| instance(tz, n)),
                });
                let until_ms = until.map(|u| u.timestamp_millis());
                if until_ms.is_some_and(|u| u < lo_ms) {
                    continue;
                }
                let mut seen = 0u64;
                let r = recur::expand(rule, wall, window, &mut steps, |cand| {
                    let Some(at) = instance(tz, cand) else { return Visit::Continue };
                    let at = at.timestamp_millis();
                    if at < start_ms {
                        return Visit::Continue;
                    }
                    if until_ms.is_some_and(|u| at > u) || at > hi_ms {
                        return Visit::Stop;
                    }
                    if let Some(count) = rule.count {
                        seen += 1;
                        if seen > count {
                            return Visit::Stop;
                        }
                    }
                    admit(at, &mut starts)
                });
                if r.is_err() {
                    out_of_steps = true;
                    break;
                }
            }
        }
        'rdates: for p in &ev.rdates {
            for w in ctx.values(p) {
                if out_of_steps || steps.take(1).is_err() {
                    out_of_steps = true;
                    break 'rdates;
                }
                if let Some(d) = ctx.instant(w).map(|d| d.timestamp_millis()) {
                    if d <= hi_ms {
                        admit(d, &mut starts);
                    }
                }
            }
        }
        // The event's steps come out of the calendar's (never more than it had left).
        let _ = self.steps.take(steps.used);
        let t = bounded(&title);
        if capped {
            self.limits.push(format!(
                "event {t:?} has more than {MAX_INSTANCES_PER_EVENT} occurrences between {} and \
                 {}; the later ones are ignored",
                self.from, self.to
            ));
        }
        if out_of_steps {
            self.limits.push(if event_budget < MAX_EVENT_STEPS {
                format!(
                    "event {t:?} was cut short: the calendar needs more than {MAX_STEPS} steps \
                     to expand"
                )
            } else {
                format!(
                    "event {t:?}: its recurrence needs more than {MAX_EVENT_STEPS} steps to \
                     expand (a COUNT from long ago, or a rule that rarely matches); occurrences \
                     it did not reach are ignored"
                )
            });
        }
        for at in starts {
            let orig = Timestamp::from_millis(at);
            if overrides.contains_key(&(uid.clone(), orig)) {
                continue;
            }
            let key = format!("{uid}@{at}");
            self.push(&uid, key, &title, ev, orig, duration);
            if self.full {
                return;
            }
        }
    }

    fn push(
        &mut self,
        uid: &str,
        key: String,
        title: &str,
        ev: &VEvent,
        start: Timestamp,
        duration: Duration,
    ) {
        let end =
            Timestamp::from_millis(start.as_millis().saturating_add(duration.num_milliseconds()));
        if end < self.from || start > self.to || self.full {
            return;
        }
        if self.out.len() >= MAX_INSTANCES {
            self.full = true;
            self.limits.push(format!(
                "the calendar has more than {MAX_INSTANCES} occurrences between {} and {}; \
                 the rest are ignored",
                self.from, self.to
            ));
            return;
        }
        self.out.push(Occurrence {
            key,
            uid: uid.to_owned(),
            title: title.to_owned(),
            location: ev.location.clone(),
            start,
            end,
        });
    }
}

fn chrono_of(t: Timestamp) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(t.as_millis())
}

/// How long an event lasts: from `DTEND` or `DURATION`, never negative, at most [`MAX_SPAN`].
fn span(
    ev: &VEvent,
    ctx: &mut Ctx<'_>,
    start_when: When,
    start: &DateTime<Utc>,
    title: &str,
) -> Duration {
    let d = if let Some(end) =
        ev.dtend.as_ref().and_then(|p| ctx.value(p)).and_then(|w| ctx.instant(w))
    {
        end.signed_duration_since(start)
    } else if let Some(d) = ev.duration.as_deref().and_then(parse_duration) {
        d
    } else if matches!(start_when, When::Date(_)) {
        // RFC 5545: an all-day event without an end lasts the day; a timed one is an instant.
        Duration::days(1)
    } else {
        Duration::zero()
    };
    if d > MAX_SPAN {
        ctx.warnings.insert(format!(
            "event {:?} lasts longer than {} days; it is read as {} days long",
            bounded(title),
            MAX_SPAN.num_days(),
            MAX_SPAN.num_days()
        ));
        return MAX_SPAN;
    }
    d.max(Duration::zero())
}

#[cfg(test)]
mod tests;
