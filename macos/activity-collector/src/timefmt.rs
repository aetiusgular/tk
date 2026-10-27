//! UTC calendar helpers: RFC 3339 parsing to epoch milliseconds and the
//! `YYYY-MM-DD` day names the log files use. No external time crate.

/// Milliseconds in one day.
pub const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's
/// `days_from_civil`). Inputs are already range-checked by the callers.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date for a count of days since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn digits(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut n: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(n)
}

fn parse_date(b: &[u8]) -> Option<(i64, u32, u32)> {
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year = i64::from(digits(&b[0..4])?);
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+HH:MM|-HH:MM)` to milliseconds
/// since the epoch. Fractions beyond milliseconds are dropped (floor).
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b.len() > 64 {
        return None;
    }
    let (year, month, day) = parse_date(&b[0..10])?;
    if b[10] != b'T' && b[10] != b't' && b[10] != b' ' {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let hour = i64::from(digits(&b[11..13])?);
    let minute = i64::from(digits(&b[14..16])?);
    let second = i64::from(digits(&b[17..19])?);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut i = 19;
    let mut millis: i64 = 0;
    if b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = &b[start..i];
        if frac.is_empty() {
            return None;
        }
        for k in 0..3 {
            let d = frac.get(k).map_or(0, |c| i64::from(c - b'0'));
            millis = millis * 10 + d;
        }
    }
    let offset_min: i64 = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let oh = i64::from(digits(&b[i + 1..i + 3])?);
            let om = i64::from(digits(&b[i + 4..i + 6])?);
            if oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 60 + om;
            if *sign == b'+' {
                total
            } else {
                -total
            }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(offset_min * 60)?;
    secs.checked_mul(1000)?.checked_add(millis)
}

/// The UTC day number (days since the epoch) of a millisecond timestamp.
pub fn day_of_ms(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

/// `YYYY-MM-DD` for a day number.
pub fn day_name(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parses a strict `YYYY-MM-DD` to a day number.
pub fn parse_day(s: &str) -> Option<i64> {
    let (y, m, d) = parse_date(s.as_bytes())?;
    Some(days_from_civil(y, m, d))
}

/// Now, in milliseconds since the epoch (0 if the clock is before 1970).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nanosecond_utc() {
        assert_eq!(
            parse_rfc3339_ms("2025-10-09T12:00:00.123456789Z"),
            Some(1_760_011_200_123)
        );
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parses_offsets() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_ms("1969-12-31T23:00:00.5-01:00"), Some(500));
    }

    #[test]
    fn refuses_garbage() {
        for s in [
            "",
            "2025-02-30T00:00:00Z",
            "2025-13-01T00:00:00Z",
            "2025-10-09T24:00:00Z",
            "2025-10-09T12:00:00",
            "2025-10-09T12:00:00.Z",
            "2025-10-09T12:00:00Zjunk",
            "2025-10-09T12:00:00+0100",
            "２025-10-09T12:00:00Z",
            "2025-1a-09T12:00:00Z",
        ] {
            assert_eq!(parse_rfc3339_ms(s), None, "{s}");
        }
    }

    #[test]
    fn leap_day() {
        assert!(parse_rfc3339_ms("2024-02-29T00:00:00Z").is_some());
        assert!(parse_rfc3339_ms("2023-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn day_names_round_trip() {
        for day in [-1, 0, 1, 19_000, 20_371, 2_932_896] {
            assert_eq!(parse_day(&day_name(day)), Some(day));
        }
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(day_name(day_of_ms(1_760_011_200_123)), "2025-10-09");
        assert_eq!(day_of_ms(-1), -1);
        assert_eq!(parse_day("2025-10-9"), None);
    }
}
