//! RFC 5545 recurrence rules (`RRULE`), expanded one period at a time under a step budget.
//!
//! The semantics are RFC 5545's as python-dateutil and the `rrule` crate implement them (the
//! tests check this module against that crate): a rule walks periods of `INTERVAL` × `FREQ` from
//! `DTSTART`, keeps the days and times its `BY…` parts allow (defaults come from `DTSTART`),
//! picks `BYSETPOS` within each period, and stops at `COUNT` or `UNTIL`. Times are local wall
//! times in the start's zone; turning them into instants is the caller's job. One deliberate
//! difference: `BYDAY` mixing plain and numbered weekdays (`MO,1TU`) is their union, as the RFC
//! says; the crate intersects them.
//!
//! **Cost.** The crate generates every occurrence from `DTSTART` (a rule `FREQ=SECONDLY` from
//! 1900 does not finish) and cannot be bounded: a rule that never matches loops 100,000 periods
//! per call. Here:
//! - Without `COUNT`, the walk starts at the period containing the window's start, computed
//!   arithmetically, so a daily rule since 2010 costs a few steps. With `COUNT` the occurrences
//!   before the window must be counted, so it walks from `DTSTART`.
//! - It stops at the window's end, whether or not anything matched, so a rule that never matches
//!   costs the periods in the window, not a search into the future.
//! - Sub-daily rules skip a day, hour or minute that their `BY…` parts exclude in one step.
//! - Every period, day and candidate time costs a step from [`Steps`]; when they run out the
//!   expansion stops and says so.

use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike, Weekday};

/// Ordered from the longest period to the shortest, as in the RFC's tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Freq {
    Yearly,
    Monthly,
    Weekly,
    Daily,
    Hourly,
    Minutely,
    Secondly,
}

/// `UNTIL` as written; the caller reads it in the start's zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Until {
    Utc(NaiveDateTime),
    Local(NaiveDateTime),
    /// Inclusive: the end of that day.
    Date(NaiveDate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub freq: Freq,
    interval: i64,
    pub count: Option<u64>,
    pub until: Option<Until>,
    wkst: Weekday,
    by_month: Vec<i32>,
    by_week_no: Vec<i32>,
    by_year_day: Vec<i32>,
    by_month_day: Vec<i32>,
    /// (ordinal, weekday); ordinal 0 is every such weekday.
    by_day: Vec<(i32, Weekday)>,
    by_hour: Vec<i32>,
    by_minute: Vec<i32>,
    by_second: Vec<i32>,
    by_set_pos: Vec<i32>,
}

const MAX_INTERVAL: i64 = 1_000_000;

/// Reads `FREQ=WEEKLY;BYDAY=MO,WE`. Errors say what is wrong, for the source's health.
pub(crate) fn parse(text: &str) -> Result<Rule, String> {
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        wkst: Weekday::Mon,
        by_month: Vec::new(),
        by_week_no: Vec::new(),
        by_year_day: Vec::new(),
        by_month_day: Vec::new(),
        by_day: Vec::new(),
        by_hour: Vec::new(),
        by_minute: Vec::new(),
        by_second: Vec::new(),
        by_set_pos: Vec::new(),
    };
    let mut freq = None;
    for part in text.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (key, value) =
            part.split_once('=').ok_or_else(|| format!("{:?} is not NAME=VALUE", short(part)))?;
        let (key, value) = (key.trim().to_ascii_uppercase(), value.trim());
        match key.as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "YEARLY" => Freq::Yearly,
                    "MONTHLY" => Freq::Monthly,
                    "WEEKLY" => Freq::Weekly,
                    "DAILY" => Freq::Daily,
                    "HOURLY" => Freq::Hourly,
                    "MINUTELY" => Freq::Minutely,
                    "SECONDLY" => Freq::Secondly,
                    _ => return Err(format!("unknown FREQ {:?}", short(value))),
                });
            }
            "INTERVAL" => {
                rule.interval = value
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_INTERVAL).contains(n))
                    .ok_or_else(|| format!("INTERVAL must be 1 to {MAX_INTERVAL}"))?;
            }
            "COUNT" => {
                rule.count = Some(value.parse().map_err(|_| "COUNT must be a number")?);
            }
            "UNTIL" => rule.until = Some(parse_until(value)?),
            "WKST" => rule.wkst = weekday(value)?,
            "BYMONTH" => rule.by_month = list(value, "BYMONTH", 1, 12, false)?,
            "BYWEEKNO" => rule.by_week_no = list(value, "BYWEEKNO", 1, 53, true)?,
            "BYYEARDAY" => rule.by_year_day = list(value, "BYYEARDAY", 1, 366, true)?,
            "BYMONTHDAY" => rule.by_month_day = list(value, "BYMONTHDAY", 1, 31, true)?,
            "BYHOUR" => rule.by_hour = list(value, "BYHOUR", 0, 23, false)?,
            "BYMINUTE" => rule.by_minute = list(value, "BYMINUTE", 0, 59, false)?,
            "BYSECOND" => rule.by_second = list(value, "BYSECOND", 0, 59, false)?,
            "BYSETPOS" => rule.by_set_pos = list(value, "BYSETPOS", 1, 366, true)?,
            "BYDAY" => {
                let mut days = Vec::new();
                for item in value.split(',').map(str::trim) {
                    let split = item.len().saturating_sub(2);
                    let (n, wd) =
                        (item.get(..split).unwrap_or(""), item.get(split..).unwrap_or(""));
                    let n: i32 = match n {
                        "" => 0,
                        n => n
                            .parse()
                            .ok()
                            .filter(|n: &i32| *n != 0 && (-53..=53).contains(n))
                            .ok_or_else(|| format!("BYDAY {:?}", short(item)))?,
                    };
                    days.push((n, weekday(wd)?));
                }
                days.sort_by_key(|(n, wd)| (*n, wd.num_days_from_monday()));
                days.dedup();
                rule.by_day = days;
            }
            k if k.starts_with("X-") => {}
            k => return Err(format!("{} is not supported", short(k))),
        }
    }
    rule.freq = freq.ok_or("FREQ is missing")?;
    let f = rule.freq;
    let misplaced = if !rule.by_week_no.is_empty() && f != Freq::Yearly {
        Some("BYWEEKNO")
    } else if !rule.by_year_day.is_empty()
        && matches!(f, Freq::Monthly | Freq::Weekly | Freq::Daily)
    {
        Some("BYYEARDAY")
    } else if !rule.by_month_day.is_empty() && f == Freq::Weekly {
        Some("BYMONTHDAY")
    } else if rule.by_day.iter().any(|(n, _)| *n != 0)
        && (!matches!(f, Freq::Monthly | Freq::Yearly) || !rule.by_week_no.is_empty())
    {
        Some("BYDAY with a number")
    } else {
        None
    };
    if let Some(part) = misplaced {
        let freq = format!("{f:?}").to_uppercase();
        return Err(format!("{part} cannot be used with FREQ={freq}"));
    }
    if f == Freq::Monthly && rule.by_day.iter().any(|(n, _)| n.abs() > 5) {
        return Err("BYDAY in a monthly rule numbers at most 5 weeks".into());
    }
    if !rule.by_set_pos.is_empty()
        && rule.by_month.is_empty()
        && rule.by_week_no.is_empty()
        && rule.by_year_day.is_empty()
        && rule.by_month_day.is_empty()
        && rule.by_day.is_empty()
        && rule.by_hour.is_empty()
        && rule.by_minute.is_empty()
        && rule.by_second.is_empty()
    {
        return Err("BYSETPOS needs another BY part".into());
    }
    Ok(rule)
}

fn short(s: &str) -> String {
    s.chars().take(32).collect()
}

fn weekday(s: &str) -> Result<Weekday, String> {
    Ok(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return Err(format!("{:?} is not a weekday", short(s))),
    })
}

/// A comma list of integers in `lo..=hi` (or `-hi..=-lo` when `signed`), sorted and deduplicated,
/// so it never has more entries than its range.
fn list(s: &str, what: &str, lo: i32, hi: i32, signed: bool) -> Result<Vec<i32>, String> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim) {
        let n: i32 = item.parse().map_err(|_| format!("{what} {:?}", short(item)))?;
        if !((lo..=hi).contains(&n) || (signed && (-hi..=-lo).contains(&n))) {
            return Err(format!("{what} {n} is out of range"));
        }
        out.push(n);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn parse_until(v: &str) -> Result<Until, String> {
    let bad = || format!("UNTIL {:?}", short(v));
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").map(Until::Date).map_err(|_| bad());
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let n = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").map_err(|_| bad())?;
    Ok(if utc { Until::Utc(n) } else { Until::Local(n) })
}

/// The step budget. Shared by everything one expansion does, so its total is bounded.
#[derive(Debug)]
pub(crate) struct Steps {
    left: u64,
    pub used: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutOfSteps;

impl Steps {
    pub(crate) fn new(budget: u64) -> Self {
        Steps { left: budget, used: 0 }
    }

    pub(crate) fn left(&self) -> u64 {
        self.left
    }

    pub(crate) fn take(&mut self, n: u64) -> Result<(), OutOfSteps> {
        if self.left < n {
            self.used += self.left;
            self.left = 0;
            return Err(OutOfSteps);
        }
        self.left -= n;
        self.used += n;
        Ok(())
    }
}

/// What the visitor of an expansion wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visit {
    Continue,
    Stop,
}

/// A rule with its defaults filled from `DTSTART`, ready to walk.
struct Plan<'a> {
    rule: &'a Rule,
    start: NaiveDateTime,
    by_month: Vec<i32>,
    by_month_day: Vec<i32>,
    plain_days: Vec<Weekday>,
    numbered_days: Vec<(i32, Weekday)>,
    /// Times of day for periods of a day or longer, sorted.
    times: Vec<NaiveTime>,
    /// For sub-daily periods: allowed hours, minutes, seconds (empty = all), and the minutes and
    /// seconds an hourly or minutely period expands to.
    hours: Vec<i32>,
    minutes: Vec<i32>,
    seconds: Vec<i32>,
    /// Seconds per sub-daily period.
    step: i64,
    /// Whether a period's days must all be generated (to count them or pick positions), rather
    /// than only those inside the window.
    whole_periods: bool,
}

/// Calls `visit` with each occurrence of `rule` (local wall times in the start's zone, in order)
/// in `window` (wall times, inclusive; the caller adds the margin it needs to cover changes of
/// offset; periods of a day or longer are taken whole days at a time), plus, with `COUNT`, the
/// occurrences before the window, which it must count. The visitor applies `COUNT`, `UNTIL` and
/// the window on instants and returns `Stop` when done.
pub(crate) fn expand(
    rule: &Rule,
    start: NaiveDateTime,
    window: (NaiveDateTime, NaiveDateTime),
    steps: &mut Steps,
    mut visit: impl FnMut(NaiveDateTime) -> Visit,
) -> Result<(), OutOfSteps> {
    let plan = Plan::new(rule, start, steps)?;
    let mut p = if rule.count.is_some() { 0 } else { plan.first_period(window.0) };
    let days = (window.0.date(), window.1.date());
    loop {
        steps.take(1)?;
        let Some(period) = plan.period(p) else { return Ok(()) };
        match period {
            Period::Days(first, last) => {
                if first > days.1 {
                    return Ok(());
                }
                let (first, last) = if plan.whole_periods {
                    (first, last)
                } else {
                    (first.max(days.0), last.min(days.1))
                };
                if plan.days(first, last, steps, &mut visit)? == Visit::Stop {
                    return Ok(());
                }
                p += 1;
            }
            Period::At(t) => {
                if t > window.1 {
                    return Ok(());
                }
                match plan.sub_daily(t, steps, &mut visit)? {
                    Next::Stop => return Ok(()),
                    Next::Period => p += 1,
                    Next::After(at) => p = plan.period_at_or_after(p, t, at),
                }
            }
        }
    }
}

enum Period {
    Days(NaiveDate, NaiveDate),
    At(NaiveDateTime),
}

enum Next {
    Stop,
    Period,
    /// Skip to the first period at or after this time.
    After(NaiveDateTime),
}

fn month_len(y: i32, m: u32) -> Option<i32> {
    let first = NaiveDate::from_ymd_opt(y, m, 1)?;
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y.checked_add(1)?, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)?
    };
    i32::try_from(next.signed_duration_since(first).num_days()).ok()
}

fn year_len(y: i32) -> i32 {
    if NaiveDate::from_ymd_opt(y, 2, 29).is_some() { 366 } else { 365 }
}

fn add_days(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    d.checked_add_signed(TimeDelta::try_days(n)?)
}

fn week_start(d: NaiveDate, wkst: Weekday) -> Option<NaiveDate> {
    d.checked_sub_days(Days::new(u64::from(d.weekday().days_since(wkst))))
}

fn time(h: i32, m: i32, s: i32) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(u32::try_from(h).ok()?, u32::try_from(m).ok()?, u32::try_from(s).ok()?)
}

impl<'a> Plan<'a> {
    fn new(rule: &'a Rule, start: NaiveDateTime, steps: &mut Steps) -> Result<Self, OutOfSteps> {
        let start = start.with_nanosecond(0).unwrap_or(start);
        let mut by_month = rule.by_month.clone();
        let mut by_month_day = rule.by_month_day.clone();
        let mut by_day = rule.by_day.clone();
        if rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty()
        {
            let (m, d) = (start.month() as i32, start.day() as i32);
            match rule.freq {
                Freq::Yearly => {
                    if by_month.is_empty() {
                        by_month = vec![m];
                    }
                    by_month_day = vec![d];
                }
                Freq::Monthly => by_month_day = vec![d],
                Freq::Weekly => by_day = vec![(0, start.weekday())],
                _ => {}
            }
        }
        let default = |given: &Vec<i32>, coarser: bool, v: u32| -> Vec<i32> {
            if given.is_empty() && coarser { vec![v as i32] } else { given.clone() }
        };
        let hours = default(&rule.by_hour, rule.freq < Freq::Hourly, start.hour());
        let minutes = default(&rule.by_minute, rule.freq < Freq::Minutely, start.minute());
        let seconds = default(&rule.by_second, rule.freq < Freq::Secondly, start.second());
        let mut times = Vec::new();
        if rule.freq <= Freq::Daily {
            steps.take((hours.len() * minutes.len() * seconds.len()) as u64)?;
            for &h in &hours {
                for &m in &minutes {
                    for &s in &seconds {
                        times.extend(time(h, m, s));
                    }
                }
            }
        }
        let unit = match rule.freq {
            Freq::Hourly => 3600,
            Freq::Minutely => 60,
            _ => 1,
        };
        let plain_days = by_day.iter().filter(|(n, _)| *n == 0).map(|(_, d)| *d).collect();
        let numbered_days = by_day.iter().filter(|(n, _)| *n != 0).copied().collect();
        Ok(Plan {
            rule,
            start,
            by_month,
            by_month_day,
            plain_days,
            numbered_days,
            times,
            hours,
            minutes,
            seconds,
            step: rule.interval * unit,
            whole_periods: rule.count.is_some() || !rule.by_set_pos.is_empty(),
        })
    }

    /// The period containing `at` (or the first), found arithmetically.
    fn first_period(&self, at: NaiveDateTime) -> i64 {
        let (s, n, day) = (self.start, self.rule.interval, at.date());
        let periods = match self.rule.freq {
            Freq::Yearly => i64::from(day.year()) - i64::from(s.year()),
            Freq::Monthly => {
                (i64::from(day.year()) - i64::from(s.year())) * 12 + i64::from(day.month0())
                    - i64::from(s.month0())
            }
            Freq::Weekly => match week_start(s.date(), self.rule.wkst) {
                Some(w) => day.signed_duration_since(w).num_days() / 7,
                None => 0,
            },
            Freq::Daily => day.signed_duration_since(s.date()).num_days(),
            _ => return at.signed_duration_since(s).num_seconds().div_euclid(self.step).max(0),
        };
        periods.div_euclid(n).max(0)
    }

    fn period(&self, p: i64) -> Option<Period> {
        let (s, k) = (self.start, p.checked_mul(self.rule.interval)?);
        Some(match self.rule.freq {
            Freq::Yearly => {
                let y = i32::try_from(i64::from(s.year()).checked_add(k)?).ok()?;
                Period::Days(NaiveDate::from_ymd_opt(y, 1, 1)?, NaiveDate::from_ymd_opt(y, 12, 31)?)
            }
            Freq::Monthly => {
                let i = (i64::from(s.year()) * 12 + i64::from(s.month0())).checked_add(k)?;
                let (y, m) = (i32::try_from(i.div_euclid(12)).ok()?, (i.rem_euclid(12) + 1) as u32);
                let first = NaiveDate::from_ymd_opt(y, m, 1)?;
                Period::Days(first, add_days(first, i64::from(month_len(y, m)? - 1))?)
            }
            Freq::Weekly => {
                let w = add_days(week_start(s.date(), self.rule.wkst)?, k.checked_mul(7)?)?;
                Period::Days(w, add_days(w, 6)?)
            }
            Freq::Daily => {
                let d = add_days(s.date(), k)?;
                Period::Days(d, d)
            }
            _ => Period::At(
                s.checked_add_signed(TimeDelta::try_seconds(p.checked_mul(self.step)?)?)?,
            ),
        })
    }

    /// The first period at or after `at`, and after `p`.
    fn period_at_or_after(&self, p: i64, t: NaiveDateTime, at: NaiveDateTime) -> i64 {
        let ahead = at.signed_duration_since(t).num_seconds();
        let k = (ahead + self.step - 1).div_euclid(self.step).max(1);
        p.saturating_add(k)
    }

    /// Whether a day passes the day-level `BY…` parts. `numbered` holds the days the numbered
    /// weekdays select in this period.
    fn day_ok(&self, d: NaiveDate, numbered: &[NaiveDate], weeks: Option<&WeekNumbers>) -> bool {
        let r = self.rule;
        if !self.by_month.is_empty() && !self.by_month.contains(&(d.month() as i32)) {
            return false;
        }
        if let Some(w) = weeks {
            if !w.contains(d) {
                return false;
            }
        }
        if !r.by_year_day.is_empty() {
            let (day, len) = (d.ordinal() as i32, year_len(d.year()));
            if !r.by_year_day.iter().any(|&n| n == day || n == day - len - 1) {
                return false;
            }
        }
        if !self.by_month_day.is_empty() {
            let (day, len) = (d.day() as i32, month_len(d.year(), d.month()).unwrap_or(31));
            if !self.by_month_day.iter().any(|&n| n == day || n == day - len - 1) {
                return false;
            }
        }
        if !self.plain_days.is_empty() || !self.numbered_days.is_empty() {
            return self.plain_days.contains(&d.weekday()) || numbered.contains(&d);
        }
        true
    }

    /// The days numbered weekdays (`2TU`, `-1FR`) select in the period around `first`: within
    /// each month for a monthly rule or a yearly one with `BYMONTH`, else within the year.
    fn numbered(&self, first: NaiveDate) -> Vec<NaiveDate> {
        if self.numbered_days.is_empty() {
            return Vec::new();
        }
        let y = first.year();
        let ranges: Vec<(NaiveDate, NaiveDate)> = match self.rule.freq {
            Freq::Monthly => month_range(y, first.month()).into_iter().collect(),
            Freq::Yearly if self.by_month.is_empty() => NaiveDate::from_ymd_opt(y, 1, 1)
                .zip(NaiveDate::from_ymd_opt(y, 12, 31))
                .into_iter()
                .collect(),
            Freq::Yearly => {
                self.by_month.iter().filter_map(|&m| month_range(y, m as u32)).collect()
            }
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        for (lo, hi) in ranges {
            for &(n, wd) in &self.numbered_days {
                let d = if n > 0 {
                    let first_wd = add_days(lo, i64::from(wd.days_since(lo.weekday())));
                    first_wd.and_then(|d| add_days(d, i64::from(n - 1) * 7))
                } else {
                    let last_wd =
                        hi.checked_sub_days(Days::new(u64::from(hi.weekday().days_since(wd))));
                    last_wd.and_then(|d| add_days(d, i64::from(n + 1) * 7))
                };
                out.extend(d.filter(|d| (lo..=hi).contains(d)));
            }
        }
        out
    }

    /// One period of a day or longer, from `first` to `last` (already cut to the window unless
    /// whole periods are needed).
    fn days(
        &self,
        first: NaiveDate,
        last: NaiveDate,
        steps: &mut Steps,
        visit: &mut impl FnMut(NaiveDateTime) -> Visit,
    ) -> Result<Visit, OutOfSteps> {
        if first > last {
            return Ok(Visit::Continue);
        }
        let numbered = self.numbered(first);
        let weeks = if self.rule.by_week_no.is_empty() {
            None
        } else {
            steps.take(400)?;
            WeekNumbers::new(first.year(), self.rule.wkst, &self.rule.by_week_no)
        };
        let mut days = Vec::new();
        let mut d = first;
        loop {
            steps.take(1)?;
            if self.day_ok(d, &numbered, weeks.as_ref()) {
                days.push(d);
            }
            match d.succ_opt() {
                Some(next) if next <= last => d = next,
                _ => break,
            }
        }
        let n = self.times.len();
        if self.rule.by_set_pos.is_empty() {
            for d in days {
                for t in &self.times {
                    steps.take(1)?;
                    if visit(d.and_time(*t)) == Visit::Stop {
                        return Ok(Visit::Stop);
                    }
                }
            }
            return Ok(Visit::Continue);
        }
        // Positions in the period's sorted set, without building it.
        let total = (days.len() * n) as i64;
        let mut picked: Vec<i64> = self
            .rule
            .by_set_pos
            .iter()
            .map(|&k| if k > 0 { i64::from(k) - 1 } else { total + i64::from(k) })
            .filter(|i| (0..total).contains(i))
            .collect();
        picked.sort_unstable();
        picked.dedup();
        for i in picked {
            steps.take(1)?;
            let (day, t) = (days[(i as usize) / n], self.times[(i as usize) % n]);
            if visit(day.and_time(t)) == Visit::Stop {
                return Ok(Visit::Stop);
            }
        }
        Ok(Visit::Continue)
    }

    /// One hourly, minutely or secondly period starting at `t`.
    fn sub_daily(
        &self,
        t: NaiveDateTime,
        steps: &mut Steps,
        visit: &mut impl FnMut(NaiveDateTime) -> Visit,
    ) -> Result<Next, OutOfSteps> {
        let day = t.date();
        let next_day = || day.succ_opt().map(|d| d.and_time(NaiveTime::MIN));
        let numbered = Vec::new();
        if !self.day_ok(day, &numbered, None) {
            return Ok(next_day().map_or(Next::Stop, Next::After));
        }
        let (h, m, s) = (t.hour() as i32, t.minute() as i32, t.second() as i32);
        // A value the rule excludes: on to the next one it allows (the next period on the
        // rule's grid at or after it), or to the next hour, minute or day.
        let skip = |at: Option<NaiveDateTime>| Ok(at.map_or(Next::Stop, Next::After));
        let at = |h: i32, m: i32, s: i32| time(h, m, s).map(|x| day.and_time(x));
        if !self.hours.is_empty() && !self.hours.contains(&h) {
            return skip(match next_in(&self.hours, h) {
                Some(h) => at(h, 0, 0),
                None => next_day(),
            });
        }
        let fine = self.rule.freq >= Freq::Minutely;
        if fine && !self.minutes.is_empty() && !self.minutes.contains(&m) {
            return skip(match next_in(&self.minutes, m) {
                Some(m) => at(h, m, 0),
                None => at(h, 0, 0).and_then(|t| t.checked_add_signed(TimeDelta::hours(1))),
            });
        }
        let secondly = self.rule.freq == Freq::Secondly;
        if secondly && !self.seconds.is_empty() && !self.seconds.contains(&s) {
            return skip(match next_in(&self.seconds, s) {
                Some(s) => at(h, m, s),
                None => at(h, m, 0).and_then(|t| t.checked_add_signed(TimeDelta::minutes(1))),
            });
        }
        let mut candidates = Vec::new();
        match self.rule.freq {
            Freq::Hourly => {
                for &m in &self.minutes {
                    for &s in &self.seconds {
                        candidates.extend(time(h, m, s));
                    }
                }
            }
            Freq::Minutely => {
                for &s in &self.seconds {
                    candidates.extend(time(h, m, s));
                }
            }
            _ => candidates.extend(time(h, m, s)),
        }
        steps.take(candidates.len() as u64)?;
        let total = candidates.len() as i64;
        let picked: Vec<NaiveTime> = if self.rule.by_set_pos.is_empty() {
            candidates
        } else {
            let mut idx: Vec<i64> = self
                .rule
                .by_set_pos
                .iter()
                .map(|&k| if k > 0 { i64::from(k) - 1 } else { total + i64::from(k) })
                .filter(|i| (0..total).contains(i))
                .collect();
            idx.sort_unstable();
            idx.dedup();
            idx.into_iter().map(|i| candidates[i as usize]).collect()
        };
        for c in picked {
            if visit(day.and_time(c)) == Visit::Stop {
                return Ok(Next::Stop);
            }
        }
        Ok(Next::Period)
    }
}

/// The first value in a sorted list after `v`.
fn next_in(list: &[i32], v: i32) -> Option<i32> {
    list.iter().copied().find(|&x| x > v)
}

fn month_range(y: i32, m: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::from_ymd_opt(y, m, 1)?;
    Some((first, add_days(first, i64::from(month_len(y, m)? - 1))?))
}

/// The days of one year that `BYWEEKNO` selects, as python-dateutil computes them: week 1 is the
/// first week (starting on `WKST`) with at least four days in the year; days of the year that
/// belong to week 1 of the next year, or the last week of the previous one, count as those.
struct WeekNumbers {
    year: i32,
    /// Indexed by day of the year, 0-based, plus a week of spill.
    mask: Vec<bool>,
}

impl WeekNumbers {
    fn new(year: i32, wkst: Weekday, wanted: &[i32]) -> Option<Self> {
        let jan1 = NaiveDate::from_ymd_opt(year, 1, 1)?;
        let len = year_len(year);
        let wkst = wkst.num_days_from_monday() as i32;
        let year_wd = jan1.weekday().num_days_from_monday() as i32;
        let weekday_at = |i: i32| (year_wd + i).rem_euclid(7);
        let first_week_start = (7 - year_wd + wkst).rem_euclid(7);
        let (no1, len_ext) = if first_week_start >= 4 {
            (0, len + (year_wd - wkst).rem_euclid(7))
        } else {
            (first_week_start, len - first_week_start)
        };
        let weeks = len_ext / 7 + len_ext.rem_euclid(7) / 4;
        let mut mask = vec![false; (len + 7) as usize];
        let mark_week = |from: i32, mask: &mut Vec<bool>| {
            for j in from..from + 7 {
                if let Some(slot) = mask.get_mut(j as usize) {
                    *slot = true;
                }
                if weekday_at(j + 1) == wkst {
                    break;
                }
            }
        };
        for &w in wanted {
            let n = if w < 0 { w + weeks + 1 } else { w };
            if !(1..=weeks).contains(&n) {
                continue;
            }
            let mut i = if n > 1 { no1 + (n - 1) * 7 } else { no1 };
            if n > 1 && no1 != first_week_start {
                i -= 7 - first_week_start;
            }
            mark_week(i, &mut mask);
        }
        if wanted.contains(&1) {
            let mut i = no1 + weeks * 7;
            if no1 != first_week_start {
                i -= 7 - first_week_start;
            }
            if i < len {
                mark_week(i, &mut mask);
            }
        }
        if no1 > 0 {
            let last_weeks = if wanted.contains(&-1) {
                -1
            } else {
                let prev_jan1 = NaiveDate::from_ymd_opt(year - 1, 1, 1)?;
                let prev_wd = prev_jan1.weekday().num_days_from_monday() as i32;
                let prev_no1 = (7 - prev_wd + wkst).rem_euclid(7);
                let ext = if prev_no1 >= 4 {
                    year_len(year - 1) + (prev_wd - wkst).rem_euclid(7)
                } else {
                    len - no1
                };
                52 + ext.rem_euclid(7) / 4
            };
            if wanted.contains(&last_weeks) {
                for slot in mask.iter_mut().take(no1 as usize) {
                    *slot = true;
                }
            }
        }
        Some(WeekNumbers { year, mask })
    }

    fn contains(&self, d: NaiveDate) -> bool {
        d.year() == self.year && self.mask.get(d.ordinal0() as usize).copied().unwrap_or(false)
    }
}
