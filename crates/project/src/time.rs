//! [`Timestamp`]: an instant in UTC to the millisecond, written in project files as RFC 3339 text, such as
//! `2026-10-21T13:02:11.250Z`, so that people reading a project file (or a reproducibility sidecar) see a date.
//!
//! The conversions between days and civil dates are Howard Hinnant's algorithms from "chrono-Compatible Low-Level
//! Date Algorithms" (<https://howardhinnant.github.io/date_algorithms.html>), on the proleptic Gregorian calendar.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An instant, in milliseconds since the Unix epoch, between the years 0 and 9999.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    millis: i64,
}

/// 0000-01-01T00:00:00Z.
const MIN_MILLIS: i64 = -62_167_219_200_000;
/// 9999-12-31T23:59:59.999Z.
const MAX_MILLIS: i64 = 253_402_300_799_999;
const MILLIS_PER_DAY: i64 = 86_400_000;

/// Why a text isn't a timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a date and time like 2026-10-21T13:02:11Z (RFC 3339, years 0 to 9999)")]
pub struct TimestampError;

impl Timestamp {
    pub const UNIX_EPOCH: Timestamp = Timestamp { millis: 0 };

    /// The current time.
    pub fn now() -> Timestamp {
        Timestamp::from_system_time(SystemTime::now())
    }

    /// A system time, clamped to the years 0 to 9999.
    pub fn from_system_time(time: SystemTime) -> Timestamp {
        let millis = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |m| -m),
        };
        Timestamp { millis: millis.clamp(MIN_MILLIS, MAX_MILLIS) }
    }

    /// The instant `millis` milliseconds after the Unix epoch, if it lies in the years 0 to 9999.
    pub fn from_unix_millis(millis: i64) -> Option<Timestamp> {
        (MIN_MILLIS..=MAX_MILLIS).contains(&millis).then_some(Timestamp { millis })
    }

    /// Milliseconds since the Unix epoch.
    pub fn unix_millis(self) -> i64 {
        self.millis
    }

    pub fn to_system_time(self) -> SystemTime {
        let magnitude = Duration::from_millis(self.millis.unsigned_abs());
        if self.millis >= 0 { UNIX_EPOCH + magnitude } else { UNIX_EPOCH - magnitude }
    }
}

/// The day number (days since 1970-01-01) of a civil date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12; // March is 0.
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date (year, month, day) of a day number.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.millis.div_euclid(MILLIS_PER_DAY);
        let ms = self.millis.rem_euclid(MILLIS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let (h, m, s, frac) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
        write!(f, "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}")?;
        if frac != 0 {
            write!(f, ".{frac:03}")?;
        }
        f.write_str("Z")
    }
}

/// A run of ASCII digits as a number, or `None` if `bytes` isn't one.
fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() || bytes.len() > 9 {
        return None;
    }
    bytes.iter().try_fold(0i64, |n, &b| b.is_ascii_digit().then(|| n * 10 + i64::from(b - b'0')))
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    /// RFC 3339: `YYYY-MM-DDTHH:MM:SS`, optional fractional seconds (kept to the millisecond), then `Z` or an offset
    /// such as `+02:00`.
    fn from_str(text: &str) -> Result<Timestamp, TimestampError> {
        let b = text.as_bytes();
        let field = |range: std::ops::Range<usize>| b.get(range).and_then(digits).ok_or(TimestampError);
        let sep = |i: usize, allowed: &[u8]| b.get(i).is_some_and(|c| allowed.contains(c));
        if !(sep(4, b"-") && sep(7, b"-") && sep(10, b"Tt ") && sep(13, b":") && sep(16, b":")) {
            return Err(TimestampError);
        }
        let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
        let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
        if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) || hour > 23 || minute > 59 || second > 59 {
            return Err(TimestampError);
        }
        let mut i = 19;
        let mut millis = 0;
        if b.get(i) == Some(&b'.') {
            let start = i + 1;
            let mut end = start;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return Err(TimestampError);
            }
            // Milliseconds: the first three digits, padded; finer digits are dropped.
            for k in 0..3 {
                millis = millis * 10 + b.get(start + k).filter(|_| start + k < end).map_or(0, |d| i64::from(d - b'0'));
            }
            i = end;
        }
        let offset_minutes = match b.get(i..) {
            Some(b"Z" | b"z") => 0,
            Some(rest @ [sign @ (b'+' | b'-'), _, _, b':', _, _]) => {
                let (h, m) = (rest.get(1..3).and_then(digits).ok_or(TimestampError)?, rest.get(4..6).and_then(digits).ok_or(TimestampError)?);
                if h > 23 || m > 59 {
                    return Err(TimestampError);
                }
                if *sign == b'+' { h * 60 + m } else { -(h * 60 + m) }
            }
            _ => return Err(TimestampError),
        };
        let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
        Timestamp::from_unix_millis(seconds * 1000 + millis).ok_or(TimestampError)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|e| serde::de::Error::custom(format!("{text:?} is {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn known_instants_read_and_write_as_rfc_3339() {
        assert_eq!(Timestamp::UNIX_EPOCH.to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(ts("1970-01-01T00:00:00Z"), Timestamp::UNIX_EPOCH);
        // 2026-10-21T13:02:11.250Z, worked out by hand: 20747 days, then 13:02:11.25.
        let t = Timestamp::from_unix_millis(20_747 * MILLIS_PER_DAY + (13 * 3600 + 2 * 60 + 11) * 1000 + 250).unwrap();
        assert_eq!(t.to_string(), "2026-10-21T13:02:11.250Z");
        assert_eq!(ts("2026-10-21T13:02:11.250Z"), t);
        assert_eq!(ts("2026-10-21T15:02:11.25+02:00"), t, "offsets are applied");
        assert_eq!(ts("2026-10-21t13:02:11.250999z"), t, "finer digits are dropped");
        assert_eq!(ts("2000-02-29T00:00:00Z").to_string(), "2000-02-29T00:00:00Z");
        assert_eq!(ts("1969-12-31T23:59:59.999Z").unix_millis(), -1);
        assert_eq!(ts("0000-01-01T00:00:00Z").unix_millis(), MIN_MILLIS);
        assert_eq!(ts("9999-12-31T23:59:59.999Z").unix_millis(), MAX_MILLIS);
    }

    #[test]
    fn every_day_of_four_centuries_round_trips() {
        let start = days_from_civil(1800, 1, 1);
        for days in start..start + 146_097 {
            let (y, m, d) = civil_from_days(days);
            assert!((1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d), "{y}-{m}-{d}");
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn malformed_text_is_an_error() {
        for bad in [
            "",
            "2026-10-21",
            "2026-10-21T13:02:11",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-21T24:00:00Z",
            "2026-10-21T13:60:00Z",
            "2026-10-21T13:02:11.Z",
            "2026-10-21T13:02:11+2:00",
            "2026-10-21T13:02:11+24:00",
            "2026-10-21T13:02:11Zjunk",
            "+2026-10-21T13:02:11Z",
            "2026/10/21T13:02:11Z",
            "２０２６-10-21T13:02:11Z",
            "9999-12-31T23:59:59-01:00",
        ] {
            assert_eq!(bad.parse::<Timestamp>(), Err(TimestampError), "{bad:?}");
        }
    }

    #[test]
    fn system_times_convert_and_clamp() {
        let t = Timestamp::from_system_time(UNIX_EPOCH + Duration::from_millis(1_792_500_000_123));
        assert_eq!(t.unix_millis(), 1_792_500_000_123);
        assert_eq!(t.to_system_time(), UNIX_EPOCH + Duration::from_millis(1_792_500_000_123));
        assert_eq!(Timestamp::from_system_time(UNIX_EPOCH - Duration::from_secs(1)).unix_millis(), -1000);
        assert_eq!(Timestamp::from_unix_millis(MAX_MILLIS + 1), None);
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"2026-10-20T12:40:00.123Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), t);
        assert!(serde_json::from_str::<Timestamp>("1792500000").is_err());
    }
}
