//! Timestamps and durations.
//!
//! `Timestamp` is milliseconds since the Unix epoch, serialized as an RFC 3339 string in UTC with
//! exactly three fractional digits (`2026-11-16T09:41:07.123Z`), so the string form sorts like the
//! number. Nothing here reads the clock: callers convert `SystemTime` themselves.
//!
//! `Duration` is what config files write as `"10s"`, `"15m"`, `"1h30m"` or `"500ms"`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

/// A point in time, in milliseconds since the Unix epoch (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, TS)]
#[ts(type = "string")]
pub struct Timestamp(i64);

impl Timestamp {
    pub const EPOCH: Timestamp = Timestamp(0);

    pub const fn from_millis(ms: i64) -> Self {
        Timestamp(ms)
    }

    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Converts a `SystemTime` the caller read. Times before the epoch saturate to the epoch.
    pub fn from_system_time(t: std::time::SystemTime) -> Self {
        match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => Timestamp(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
            Err(_) => Timestamp::EPOCH,
        }
    }

    pub fn saturating_add(self, d: Duration) -> Self {
        Timestamp(self.0.saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
    }

    pub fn saturating_sub(self, d: Duration) -> Self {
        Timestamp(self.0.saturating_sub(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
    }

    /// `self - earlier`, or zero if `earlier` is later.
    pub fn since(self, earlier: Timestamp) -> Duration {
        Duration::from_millis(u64::try_from(self.0.saturating_sub(earlier.0)).unwrap_or(0))
    }

    /// Parses RFC 3339 (`2026-11-16T09:41:07.123Z`, `2026-11-16T10:41:07+01:00`). Fractional
    /// digits beyond milliseconds are truncated.
    pub fn parse_rfc3339(s: &str) -> Result<Self, TimeParseError> {
        parse_rfc3339(s).ok_or_else(|| TimeParseError::Timestamp(s.to_owned()))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.rem_euclid(1000);
        let secs = self.0.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
            sod / 3600,
            (sod / 60) % 60,
            sod % 60
        )
    }
}

impl FromStr for Timestamp {
    type Err = TimeParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Timestamp::parse_rfc3339(s)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(d)?;
        Timestamp::parse_rfc3339(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeParseError {
    #[error("expected an RFC 3339 timestamp like \"2026-11-16T09:41:07Z\", got {0:?}")]
    Timestamp(String),
    #[error("expected a duration like \"500ms\", \"10s\", \"15m\", \"1h30m\" or \"7d\", got {0:?}")]
    Duration(String),
}

// Howard Hinnant's days_from_civil / civil_from_days, proleptic Gregorian.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(r)?;
        if part.is_empty() || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&mo) || d < 1 || d > mdays[usize::try_from(mo - 1).ok()?] {
        return None;
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
        let digits = &s[start..i.min(start + 3)];
        ms = digits.parse::<i64>().ok()? * 10_i64.pow(3 - u32::try_from(digits.len()).ok()?);
    }
    let offset_min = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let (oh, om) = (num(i + 1..i + 3)?, num(i + 4..i + 6)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let o = oh * 60 + om;
            if *sign == b'-' { -o } else { o }
        }
        _ => return None,
    };
    let secs =
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec.min(59) - offset_min * 60;
    Some(Timestamp(secs * 1000 + ms))
}

/// A non-negative span of time, written in config as `"500ms"`, `"10s"`, `"15m"`, `"1h30m"`,
/// `"7d"`. Serialized in the same canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, TS)]
#[ts(type = "string")]
pub struct Duration(u64);

impl Duration {
    pub const ZERO: Duration = Duration(0);

    pub const fn from_millis(ms: u64) -> Self {
        Duration(ms)
    }
    pub const fn from_secs(s: u64) -> Self {
        Duration(s.saturating_mul(1000))
    }
    pub const fn from_mins(m: u64) -> Self {
        Duration(m.saturating_mul(60_000))
    }
    pub const fn from_hours(h: u64) -> Self {
        Duration(h.saturating_mul(3_600_000))
    }
    pub const fn from_days(d: u64) -> Self {
        Duration(d.saturating_mul(86_400_000))
    }
    pub const fn as_millis(self) -> u64 {
        self.0
    }
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
    pub fn to_std(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0)
    }

    pub fn parse(s: &str) -> Result<Self, TimeParseError> {
        let err = || TimeParseError::Duration(s.to_owned());
        let t = s.trim();
        if t.is_empty() {
            return Err(err());
        }
        if t == "0" {
            return Ok(Duration::ZERO);
        }
        let mut total: u64 = 0;
        let mut rest = t;
        // Units must appear largest first, each at most once, so "1h30m" is fine and "30m1h" is
        // rejected as a likely typo.
        let mut last_rank = usize::MAX;
        while !rest.is_empty() {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(err());
            }
            let n: u64 = rest[..digits].parse().map_err(|_| err())?;
            rest = &rest[digits..];
            let unit_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let (rank, mult) = match &rest[..unit_len] {
                "d" => (4, 86_400_000),
                "h" => (3, 3_600_000),
                "m" => (2, 60_000),
                "s" => (1, 1000),
                "ms" => (0, 1),
                _ => return Err(err()),
            };
            if rank >= last_rank {
                return Err(err());
            }
            last_rank = rank;
            rest = &rest[unit_len..];
            total = n.checked_mul(mult).and_then(|v| total.checked_add(v)).ok_or_else(err)?;
        }
        Ok(Duration(total))
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("0s");
        }
        let mut rest = self.0;
        for (unit, mult) in
            [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1000), ("ms", 1)]
        {
            if rest >= mult {
                write!(f, "{}{unit}", rest / mult)?;
                rest %= mult;
            }
        }
        Ok(())
    }
}

impl FromStr for Duration {
    type Err = TimeParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Duration::parse(s)
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(d)?;
        Duration::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_round_trips_and_parses_offsets() {
        let cases = [
            ("1970-01-01T00:00:00.000Z", 0),
            ("2026-11-16T09:41:07.123Z", 1_794_822_067_123),
            ("2000-02-29T23:59:59.999Z", 951_868_799_999),
            ("1969-12-31T23:59:59.999Z", -1),
        ];
        for (s, ms) in cases {
            let t = Timestamp::parse_rfc3339(s).unwrap();
            assert_eq!(t.as_millis(), ms, "{s}");
            assert_eq!(t.to_string(), s);
        }
        let equivalents = [
            "2026-11-16T10:41:07.123+01:00",
            "2026-11-16T09:41:07.123456Z",
            "2026-11-16t09:41:07.123z",
            "2026-11-16T04:41:07.123-05:00",
        ];
        for s in equivalents {
            assert_eq!(Timestamp::parse_rfc3339(s).unwrap().as_millis(), 1_794_822_067_123, "{s}");
        }
        assert_eq!(Timestamp::parse_rfc3339("2026-11-16T09:41:07Z").unwrap().as_millis() % 1000, 0);
    }

    #[test]
    fn timestamp_rejects_malformed() {
        for s in [
            "",
            "2026-11-16",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-11-16T24:00:00Z",
            "2026-11-16T09:41:07",
            "2026-11-16T09:41:07.Z",
            "2026-11-16T09:41:07+0100",
            "2026-11-16T09:41:07Zjunk",
            "+026-11-16T09:41:07Z",
        ] {
            assert!(Timestamp::parse_rfc3339(s).is_err(), "{s:?} should not parse");
        }
    }

    #[test]
    fn durations() {
        let ok = [
            ("0", 0),
            ("500ms", 500),
            ("10s", 10_000),
            ("15m", 900_000),
            ("1h30m", 5_400_000),
            ("7d", 604_800_000),
            ("1d2h3m4s5ms", 93_784_005),
        ];
        for (s, ms) in ok {
            let d = Duration::parse(s).unwrap();
            assert_eq!(d.as_millis(), ms, "{s}");
            assert_eq!(Duration::parse(&d.to_string()).unwrap(), d, "canonical form of {s}");
        }
        for s in ["", "10", "s", "10 s", "10sec", "-5s", "30m1h", "1h1h", "99999999999999999999d"] {
            assert!(Duration::parse(s).is_err(), "{s:?} should not parse");
        }
    }
}
