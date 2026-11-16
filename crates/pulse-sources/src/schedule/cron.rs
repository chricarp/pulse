//! Five-field cron (`minute hour day-of-month month day-of-week`) evaluated in a time zone.
//!
//! Fields take `*`, numbers, ranges `a-b`, steps `*/n` and `a-b/n` (and `a/n`, meaning `a` to the
//! field's maximum), comma lists, and English names for months and weekdays. Day-of-week is
//! 0-7 with both 0 and 7 meaning Sunday. As in classic cron, when both day fields are
//! restricted a day matches if either does.
//!
//! Daylight saving time, decided once and tested: a local time that does not exist (skipped by
//! a spring-forward) fires at the first instant after the gap; a local time that happens twice
//! (fall-back) fires once, at its first occurrence. So `30 2 * * *` fires at 03:00 on the
//! spring-forward day and once on the fall-back day, and an hourly schedule does not fire twice
//! in the repeated hour.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cron {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    days_restricted: bool,
    weekdays_restricted: bool,
}

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    pub(crate) fn parse(s: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        let [mi, h, dom, mo, dow] = fields[..] else {
            return Err(format!("expected five fields (minute hour day month weekday), got {s:?}"));
        };
        let weekdays = field(dow, 0, 7, &WEEKDAYS, 0, "day of week")?;
        // 7 is Sunday too.
        let weekdays = (weekdays | (weekdays >> 7)) & 0x7f;
        Ok(Cron {
            minutes: field(mi, 0, 59, &[], 0, "minute")?,
            hours: u32::try_from(field(h, 0, 23, &[], 0, "hour")?).map_err(|e| e.to_string())?,
            days: u32::try_from(field(dom, 1, 31, &[], 0, "day of month")?)
                .map_err(|e| e.to_string())?,
            months: u16::try_from(field(mo, 1, 12, &MONTHS, 1, "month")?)
                .map_err(|e| e.to_string())?,
            weekdays: u8::try_from(weekdays).map_err(|e| e.to_string())?,
            days_restricted: dom != "*",
            weekdays_restricted: dow != "*",
        })
    }

    fn day_matches(&self, d: NaiveDate) -> bool {
        let dom = self.days & (1 << d.day()) != 0;
        let dow = self.weekdays & (1 << d.weekday().num_days_from_sunday()) != 0;
        match (self.days_restricted, self.weekdays_restricted) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }

    /// The first fire strictly after `after`, searching up to eight years ahead.
    pub(crate) fn next_after<Z: TimeZone>(
        &self,
        tz: &Z,
        after: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let local = after.with_timezone(tz).naive_local();
        let mut t = local.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let limit = local + Duration::days(366 * 8);
        while t < limit {
            if self.months & (1 << t.month()) == 0 {
                let (y, m) =
                    if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) };
                t = NaiveDate::from_ymd_opt(y, m, 1)?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.day_matches(t.date()) {
                t = t.date().succ_opt()?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if self.hours & (1 << t.hour()) == 0 {
                t = t.with_minute(0)? + Duration::hours(1);
                continue;
            }
            if self.minutes & (1 << t.minute()) == 0 {
                t += Duration::minutes(1);
                continue;
            }
            if let Some(at) = resolve(tz, t) {
                if at > after {
                    return Some(at);
                }
            }
            t += Duration::minutes(1);
        }
        None
    }
}

/// A local time as an instant: the earlier of two, or for a time skipped by a transition the
/// first valid minute after the gap.
fn resolve<Z: TimeZone>(tz: &Z, t: NaiveDateTime) -> Option<DateTime<Utc>> {
    let mut probe = t;
    // Real gaps are at most a few hours; a day is a generous bound.
    for _ in 0..(24 * 60) {
        match tz.from_local_datetime(&probe) {
            LocalResult::Single(dt) => return Some(dt.with_timezone(&Utc)),
            LocalResult::Ambiguous(a, _) => return Some(a.with_timezone(&Utc)),
            LocalResult::None => probe += Duration::minutes(1),
        }
    }
    None
}

/// One field as a bit set over `min..=max`. `name_base` is the value of `names[0]`.
fn field(
    s: &str,
    min: u32,
    max: u32,
    names: &[&str],
    name_base: u32,
    what: &str,
) -> Result<u64, String> {
    let err = |detail: &str| format!("invalid {what} field {s:?}: {detail}");
    let value = |v: &str| -> Result<u32, String> {
        let lower = v.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == lower) {
            return Ok(name_base + u32::try_from(i).unwrap_or(0));
        }
        let n: u32 = v.parse().map_err(|_| err(&format!("{v:?} is not a number")))?;
        if n < min || n > max {
            return Err(err(&format!("{n} is outside {min}-{max}")));
        }
        Ok(n)
    };
    let mut bits = 0u64;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let st: u32 = st.parse().map_err(|_| err("the step must be a number"))?;
                if st == 0 {
                    return Err(err("the step must be positive"));
                }
                (r, st)
            }
            None => (part, 1),
        };
        let (lo, hi) = match range {
            "*" => (min, max),
            r => match r.split_once('-') {
                Some((a, b)) => (value(a)?, value(b)?),
                None if part.contains('/') => (value(r)?, max),
                None => {
                    let v = value(r)?;
                    (v, v)
                }
            },
        };
        if lo > hi {
            return Err(err("a range must go from low to high"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use chrono_tz::Tz;

    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parse_errors() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
            "* * * foo *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Cron::parse("0 9 * jan-mar mon-fri").is_ok());
        assert!(Cron::parse("*/15 0,12 1-7 * 7").is_ok());
    }

    #[test]
    fn next_fires() {
        let rome: Tz = "Europe/Rome".parse().unwrap();
        let ny: Tz = "America/New_York".parse().unwrap();
        // (cron, zone, after, expected next fires in order)
        let cases: Vec<(&str, Tz, &str, Vec<&str>)> = vec![
            (
                "0 9 * * 1-5",
                rome,
                "2026-11-13T09:00:00+01:00",
                vec!["2026-11-16T09:00:00+01:00", "2026-11-17T09:00:00+01:00"],
            ),
            (
                "*/20 * * * *",
                rome,
                "2026-11-16T10:05:00+01:00",
                vec![
                    "2026-11-16T10:20:00+01:00",
                    "2026-11-16T10:40:00+01:00",
                    "2026-11-16T11:00:00+01:00",
                ],
            ),
            (
                "0 0 1 jan,jul *",
                ny,
                "2026-11-16T00:00:00-05:00",
                vec!["2027-01-01T00:00:00-05:00", "2027-07-01T00:00:00-04:00"],
            ),
            // Both day fields restricted: either matches (the 13th, or any Friday).
            (
                "0 12 13 * 5",
                rome,
                "2026-11-12T00:00:00+01:00",
                vec![
                    "2026-11-13T12:00:00+01:00",
                    "2026-11-20T12:00:00+01:00",
                    "2026-11-27T12:00:00+01:00",
                    "2026-12-04T12:00:00+01:00",
                ],
            ),
            // Sunday as 7.
            ("0 8 * * 7", rome, "2026-11-16T00:00:00+01:00", vec!["2026-11-22T08:00:00+01:00"]),
            // Leap day.
            ("0 0 29 2 *", rome, "2026-03-01T00:00:00+01:00", vec!["2028-02-29T00:00:00+01:00"]),
            // Spring forward in Rome (2026-03-29, 02:00 -> 03:00): 02:30 does not exist and fires
            // when the gap ends; the next day is normal.
            (
                "30 2 * * *",
                rome,
                "2026-03-28T03:00:00+01:00",
                vec!["2026-03-29T03:00:00+02:00", "2026-03-30T02:30:00+02:00"],
            ),
            // Hourly across it: no duplicate at 03:00.
            (
                "0 * * * *",
                rome,
                "2026-03-29T00:30:00+01:00",
                vec![
                    "2026-03-29T01:00:00+01:00",
                    "2026-03-29T03:00:00+02:00",
                    "2026-03-29T04:00:00+02:00",
                ],
            ),
            // Fall back in Rome (2026-10-25, 03:00 -> 02:00): 02:30 happens twice and fires once.
            (
                "30 2 * * *",
                rome,
                "2026-10-24T03:00:00+02:00",
                vec!["2026-10-25T02:30:00+02:00", "2026-10-26T02:30:00+01:00"],
            ),
            // Hourly across it: the repeated hour fires once.
            (
                "0 * * * *",
                rome,
                "2026-10-25T00:30:00+02:00",
                vec![
                    "2026-10-25T01:00:00+02:00",
                    "2026-10-25T02:00:00+02:00",
                    "2026-10-25T03:00:00+01:00",
                ],
            ),
            // Half-hourly in the 01:00 hour, across New York's fall-back (2026-11-01, 02:00 EDT -> 01:00 EST).
            (
                "*/30 1 * * *",
                ny,
                "2026-11-01T00:59:00-04:00",
                vec![
                    "2026-11-01T01:00:00-04:00",
                    "2026-11-01T01:30:00-04:00",
                    "2026-11-02T01:00:00-05:00",
                ],
            ),
        ];
        for (expr, tz, after, want) in cases {
            let cron = Cron::parse(expr).unwrap();
            let mut t = utc(after);
            for w in &want {
                let got = cron.next_after(&tz, t).unwrap();
                assert_eq!(got, utc(w), "{expr} in {tz} after {t}");
                t = got;
            }
        }
        // Never: the 31st of February.
        assert_eq!(
            Cron::parse("0 0 31 2 *").unwrap().next_after(&rome, utc("2026-01-01T00:00:00Z")),
            None
        );
    }
}
