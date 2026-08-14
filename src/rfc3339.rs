//! RFC 3339 timestamp → epoch milliseconds.
//!
//! MiniMax and Z.ai report window boundaries as epoch milliseconds. Anthropic
//! reports them as RFC 3339 strings (`2026-08-14T14:40:00.255510+00:00`), so one
//! of the two has to be converted, and epoch milliseconds is what [`crate::pace`]
//! already speaks.
//!
//! This is hand-rolled rather than pulled from `chrono`/`time` because the whole
//! job is one grammar with a fixed shape, and a date library is a large
//! dependency for a binary built at `opt-level = "z"`. (`toml_datetime` is
//! already in the tree transitively, but it parses into calendar fields and
//! offers no epoch conversion, so it would not save the arithmetic below.)
//!
//! The date arithmetic is Howard Hinnant's `days_from_civil`, exact across the
//! whole proleptic Gregorian range with no lookup tables. Leap *seconds* are
//! deliberately not modelled: Unix time cannot represent them, and a quota
//! window is not measured to the second anyway.

/// Days from 1970-01-01 to `y-m-d` in the proleptic Gregorian calendar.
///
/// Shifts the year to start in March, which moves the leap day to the end and
/// takes February out of the middle of the arithmetic entirely.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parse an RFC 3339 timestamp into milliseconds since the Unix epoch.
///
/// Accepts the forms Anthropic actually emits — `Z`, `+00:00`, and fractional
/// seconds of any precision — and returns `None` for anything it cannot read
/// rather than guessing. A guessed timestamp becomes a silently wrong pace,
/// whereas a missing window renders as a visible `--`.
pub fn to_epoch_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();

    // Fixed-width prefix: YYYY-MM-DDTHH:MM:SS
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { s.get(range)?.parse::<i64>().ok() };

    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    // RFC 3339 permits a lowercase `t`, and a space in its relaxed form.
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }

    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // 60 is a leap second: clamp onto :59 rather than discard the timestamp,
    // since Unix time has no representation for it either way.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut rest = &s[19..];

    // Fractional seconds, truncated to milliseconds. Truncating rather than
    // rounding keeps a reset from ever being reported a millisecond late.
    let mut millis = 0i64;
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits: String = stripped.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let mut ms: String = digits.chars().take(3).collect();
        while ms.len() < 3 {
            ms.push('0');
        }
        millis = ms.parse().ok()?;
        rest = &rest[1 + digits.len()..];
    }

    // Offset. RFC 3339 requires one; without it the result would depend on the
    // reader's timezone, so it is rejected rather than assumed.
    let offset_secs = match rest.as_bytes().first() {
        Some(b'Z' | b'z') if rest.len() == 1 => 0,
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            let body = &rest[1..];
            // `+HH:MM` and the colon-less `+HHMM` both occur in the wild.
            let (h, m) = match body.len() {
                5 if body.as_bytes()[2] == b':' => (body.get(0..2)?, body.get(3..5)?),
                4 => (body.get(0..2)?, body.get(2..4)?),
                _ => return None,
            };
            let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
            if h > 23 || m > 59 {
                return None;
            }
            sign * (h * 3600 + m * 60)
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second.min(59) - offset_secs;
    secs.checked_mul(1000)?.checked_add(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_itself_is_zero() {
        assert_eq!(to_epoch_ms("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parses_the_shape_anthropic_actually_sends() {
        // Captured live from the usage endpoint.
        assert_eq!(
            to_epoch_ms("2026-08-14T14:40:00.255510+00:00"),
            Some(1_786_718_400_255)
        );
    }

    #[test]
    fn z_and_an_explicit_zero_offset_name_the_same_instant() {
        assert_eq!(
            to_epoch_ms("2026-08-17T04:00:00Z"),
            to_epoch_ms("2026-08-17T04:00:00+00:00")
        );
    }

    #[test]
    fn a_nonzero_offset_shifts_the_instant() {
        // 09:00 at +09:00 is midnight UTC; both name one instant.
        assert_eq!(
            to_epoch_ms("2026-08-14T09:00:00+09:00"),
            to_epoch_ms("2026-08-14T00:00:00Z")
        );
        assert_eq!(
            to_epoch_ms("2026-08-13T19:00:00-05:00"),
            to_epoch_ms("2026-08-14T00:00:00Z")
        );
    }

    #[test]
    fn the_colon_less_offset_form_is_accepted() {
        assert_eq!(
            to_epoch_ms("2026-08-14T09:00:00+0900"),
            to_epoch_ms("2026-08-14T00:00:00Z")
        );
    }

    #[test]
    fn fractional_seconds_truncate_rather_than_round() {
        // Rounding .999999 up would report the reset one millisecond late.
        assert_eq!(to_epoch_ms("1970-01-01T00:00:00.999999Z"), Some(999));
        assert_eq!(to_epoch_ms("1970-01-01T00:00:00.1Z"), Some(100));
        assert_eq!(to_epoch_ms("1970-01-01T00:00:00.25Z"), Some(250));
    }

    #[test]
    fn leap_days_are_counted() {
        let feb29 = to_epoch_ms("2024-02-29T00:00:00Z").unwrap();
        let mar01 = to_epoch_ms("2024-03-01T00:00:00Z").unwrap();
        assert_eq!(mar01 - feb29, 86_400_000);
    }

    #[test]
    fn the_gregorian_century_rules_are_respected() {
        // A naive %4 leap rule gets 1900 wrong and silently shifts every date
        // before it by a day.
        let a = to_epoch_ms("1900-02-28T00:00:00Z").unwrap();
        let b = to_epoch_ms("1900-03-01T00:00:00Z").unwrap();
        assert_eq!(b - a, 86_400_000, "1900 must not have a Feb 29");

        let c = to_epoch_ms("2000-02-28T00:00:00Z").unwrap();
        let d = to_epoch_ms("2000-03-01T00:00:00Z").unwrap();
        assert_eq!(d - c, 2 * 86_400_000, "2000 must have a Feb 29");
    }

    #[test]
    fn pre_epoch_timestamps_go_negative_rather_than_wrapping() {
        assert_eq!(to_epoch_ms("1969-12-31T23:59:59Z"), Some(-1000));
    }

    #[test]
    fn consecutive_days_are_exactly_one_day_apart() {
        let a = to_epoch_ms("2026-08-14T00:00:00Z").unwrap();
        let b = to_epoch_ms("2026-08-15T00:00:00Z").unwrap();
        assert_eq!(b - a, 86_400_000);
    }

    #[test]
    fn a_missing_offset_is_rejected_rather_than_assumed_local() {
        // Assuming local time would make the pace depend on $TZ, so a window
        // would read differently on two machines watching one account.
        assert_eq!(to_epoch_ms("2026-08-14T14:40:00"), None);
        assert_eq!(to_epoch_ms("2026-08-14T14:40:00.255510"), None);
    }

    #[test]
    fn malformed_input_yields_none_rather_than_a_wrong_instant() {
        for bad in [
            "",
            "not a timestamp",
            "2026-08-14",
            "2026-08-14T14:40Z",
            "2026/08/14T14:40:00Z",
            "2026-08-14T14:40:00X",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-08-32T00:00:00Z",
            "2026-08-14T24:00:00Z",
            "2026-08-14T14:60:00Z",
            "2026-08-14T14:40:00.Z",
            "2026-08-14T14:40:00+99:00",
            "2026-08-14T14:40:00+1:00",
        ] {
            assert_eq!(to_epoch_ms(bad), None, "expected None for {bad:?}");
        }
    }

    #[test]
    fn a_leap_second_clamps_instead_of_dropping_the_window() {
        // Unix time cannot represent :60; rejecting outright would lose an
        // otherwise usable reset time.
        assert_eq!(
            to_epoch_ms("2016-12-31T23:59:60Z"),
            to_epoch_ms("2016-12-31T23:59:59Z")
        );
    }

    #[test]
    fn lowercase_t_and_z_are_accepted() {
        assert_eq!(
            to_epoch_ms("2026-08-14t14:40:00z"),
            to_epoch_ms("2026-08-14T14:40:00Z")
        );
    }

    #[test]
    fn civil_days_match_known_reference_points() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }
}
