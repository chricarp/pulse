//! What a `schedule` source's settings mean, parsed once for both the validator and the source:
//! five-field cron and time zone names. Evaluating a cron in a time zone (the next fire) is the
//! source's (`pulse_sources::schedule::cron`); this only decides what is valid.
//!
//! Fields take `*`, numbers, ranges `a-b`, steps `*/n` and `a-b/n` (and `a/n`, meaning `a` to the
//! field's maximum), comma lists, and English names for months and weekdays. Day-of-week is
//! 0-7 with both 0 and 7 meaning Sunday. As in classic cron, when both day fields are
//! restricted a day matches if either does.

/// A parsed cron expression: one bit set per field, bit `n` for value `n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    pub minutes: u64,
    pub hours: u32,
    /// Days of the month, bits 1-31.
    pub days: u32,
    /// Months, bits 1-12.
    pub months: u16,
    /// Days of the week, bits 0-6, 0 being Sunday.
    pub weekdays: u8,
    pub days_restricted: bool,
    pub weekdays_restricted: bool,
}

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
/// The longest each month can be (February in a leap year).
const MONTH_DAYS: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

impl Cron {
    pub fn parse(s: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        let [mi, h, dom, mo, dow] = fields[..] else {
            return Err(format!("expected five fields (minute hour day month weekday), got {s:?}"));
        };
        let weekdays = field(dow, 0, 7, &WEEKDAYS, 0, "day of week")?;
        // 7 is Sunday too.
        let weekdays = (weekdays | (weekdays >> 7)) & 0x7f;
        let cron = Cron {
            minutes: field(mi, 0, 59, &[], 0, "minute")?,
            hours: u32::try_from(field(h, 0, 23, &[], 0, "hour")?).map_err(|e| e.to_string())?,
            days: u32::try_from(field(dom, 1, 31, &[], 0, "day of month")?)
                .map_err(|e| e.to_string())?,
            months: u16::try_from(field(mo, 1, 12, &MONTHS, 1, "month")?)
                .map_err(|e| e.to_string())?,
            weekdays: u8::try_from(weekdays).map_err(|e| e.to_string())?,
            days_restricted: dom != "*",
            weekdays_restricted: dow != "*",
        };
        if !cron.can_fire() {
            return Err(format!("{s:?} never fires: none of its months has that day"));
        }
        Ok(cron)
    }

    /// Whether any date matches. Every field has at least one value, and a day-of-week
    /// restriction matches in every month, so only a day-of-month restriction on its own can
    /// leave nothing (`0 0 30 2 *`). The 29th of February counts: it comes every four years.
    fn can_fire(&self) -> bool {
        if !self.days_restricted || self.weekdays_restricted {
            return true;
        }
        (1..=12u32).zip(MONTH_DAYS).any(|(m, last)| {
            self.months & (1 << m) != 0 && (1..=last).any(|d| self.days & (1 << d) != 0)
        })
    }
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
                // A larger step only ever matches the start of its range, which is never what
                // `*/90` meant.
                let span = max - min + 1;
                if st > span {
                    return Err(err(&format!(
                        "the step must be at most {span} (for longer intervals use `every`)"
                    )));
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
            let Some(next) = v.checked_add(step) else { break };
            v = next;
        }
    }
    Ok(bits)
}

/// An IANA time zone name, as `timezone` takes it.
pub fn parse_time_zone(name: &str) -> Result<chrono_tz::Tz, String> {
    name.parse::<chrono_tz::Tz>()
        .map_err(|_| format!("unknown time zone {name:?}; use an IANA name like \"Europe/Rome\""))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "*/61 * * * *",
            "0 0 */32 * *",
            "0 0 * */13 *",
            // Never: the 30th and 31st of February, the 31st of the short months.
            "0 0 30,31 2 *",
            "0 0 31 apr,jun,sep,nov *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?}");
        }
        for good in [
            "0 9 * jan-mar mon-fri",
            "*/15 0,12 1-7 * 7",
            "*/60 * * * *",
            // A leap day fires every four years; the 31st of February on a Friday fires on
            // Fridays (either day field matches).
            "0 0 29 2 *",
            "0 0 31 2 fri",
        ] {
            assert!(Cron::parse(good).is_ok(), "{good:?}");
        }
    }
}
