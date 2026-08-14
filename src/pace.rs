//! Pace ratio: the unit-free "am I burning this too fast?" signal.
//!
//! Providers report quota in incompatible units — MiniMax gives a *remaining*
//! percentage, Z.ai gives a *used* percentage, and neither exposes a usable token
//! count. Comparing them requires normalizing to two fractions:
//!
//! ```text
//! pace = (fraction of quota consumed) / (fraction of window elapsed)
//! ```
//!
//! `1.0` means the quota lands exactly at reset. `> 1.0` exhausts early.
//! `< 1.0` leaves headroom. Because both terms are fractions, a token-based
//! MiniMax quota and a percentage-based Z.ai quota produce comparable numbers.

use serde::Serialize;

/// Below this much of a window elapsed, pace is meaningless: dividing a real
/// usage fraction by a near-zero elapsed fraction produces an arbitrarily large
/// number that looks like an emergency. Report no signal instead.
const MIN_ELAPSED_FRACTION: f64 = 0.02;

/// One quota window, normalized across providers.
///
/// `used_percent` is always *consumed*, 0..=100. Providers that report remaining
/// are inverted at the parser, not here.
#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub used_percent: f64,
    /// Window boundaries in milliseconds since the Unix epoch. Both are needed
    /// for a pace signal; usage alone is still reported without them.
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl Window {
    pub fn new(used_percent: f64, start_ms: Option<i64>, end_ms: Option<i64>) -> Self {
        Self {
            used_percent,
            start_ms,
            end_ms,
        }
    }

    /// How far through the window we are, 0.0..=1.0.
    ///
    /// `None` when boundaries are missing or non-positive in length. Clamped at
    /// the top so a window whose reset is overdue cannot report < 100% of its
    /// budget spent per unit time.
    pub fn elapsed_fraction(&self, now_ms: i64) -> Option<f64> {
        let (start, end) = (self.start_ms?, self.end_ms?);
        let span = end.checked_sub(start)?;
        if span <= 0 {
            return None;
        }
        let elapsed = now_ms.saturating_sub(start);
        if elapsed <= 0 {
            return Some(0.0);
        }
        Some((elapsed as f64 / span as f64).min(1.0))
    }

    /// Pace ratio, or `None` when there is no honest signal to give.
    ///
    /// Returns `None` rather than a fabricated number when the window has just
    /// reset — see [`MIN_ELAPSED_FRACTION`].
    pub fn pace(&self, now_ms: i64) -> Option<f64> {
        let elapsed = self.elapsed_fraction(now_ms)?;
        if elapsed < MIN_ELAPSED_FRACTION {
            return None;
        }
        Some((self.used_percent / 100.0) / elapsed)
    }

    /// Milliseconds until the window resets, if known.
    pub fn resets_in_ms(&self, now_ms: i64) -> Option<i64> {
        Some(self.end_ms?.saturating_sub(now_ms).max(0))
    }
}

/// Render a duration the way it would be spoken: `2d 13h` / `4h 57m` / `45m` / `30s`.
///
/// Weekly windows run to days, so hours alone would print `86h`. Two units carry
/// all the signal a reset countdown needs — nobody changes behaviour over the
/// minutes remaining in a two-day window — so smaller ones are dropped rather
/// than concatenated.
///
/// A zero unit is omitted entirely: `7d`, not `7d 00h`. Padding a component to
/// two digits reads as a clock face, which invites the countdown to be mistaken
/// for a wall-clock reset time.
pub fn format_duration(ms: i64) -> String {
    let total_secs = ms.max(0) / 1000;
    let (d, h) = (total_secs / 86_400, (total_secs % 86_400) / 3600);
    let (m, s) = ((total_secs % 3600) / 60, total_secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, 0) => format!("{h}h"),
        (0, _, _) => format!("{h}h {m}m"),
        (_, 0, _) => format!("{d}d"),
        (_, _, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    const START: i64 = 1_786_701_600_000;

    fn window(used: f64) -> Window {
        Window::new(used, Some(START), Some(START + 5 * HOUR))
    }

    #[test]
    fn half_used_at_half_elapsed_is_exactly_on_budget() {
        let pace = window(50.0).pace(START + 150 * HOUR / 60).unwrap();
        assert!((pace - 1.0).abs() < 0.01, "expected ~1.0, got {pace}");
    }

    #[test]
    fn burning_at_double_speed_reads_above_one() {
        // 50% of quota gone with only 25% of the window elapsed.
        let pace = window(50.0).pace(START + 5 * HOUR / 4).unwrap();
        assert!((pace - 2.0).abs() < 0.01, "expected ~2.0, got {pace}");
    }

    #[test]
    fn headroom_reads_below_one() {
        let pace = window(10.0).pace(START + 5 * HOUR / 2).unwrap();
        assert!(pace < 1.0, "expected < 1.0, got {pace}");
    }

    #[test]
    fn freshly_reset_window_gives_no_signal_rather_than_a_huge_one() {
        // One second into a five-hour window. Any usage divided by this is noise.
        assert_eq!(window(3.0).pace(START + 1_000), None);
    }

    #[test]
    fn zero_usage_in_a_fresh_window_is_still_no_signal() {
        assert_eq!(window(0.0).pace(START), None);
    }

    #[test]
    fn missing_boundaries_give_no_signal() {
        assert_eq!(Window::new(40.0, None, None).pace(START + HOUR), None);
        assert_eq!(
            Window::new(40.0, Some(START), None).pace(START + HOUR),
            None
        );
    }

    #[test]
    fn zero_length_window_does_not_divide_by_zero() {
        let w = Window::new(40.0, Some(START), Some(START));
        assert_eq!(w.elapsed_fraction(START + HOUR), None);
        assert_eq!(w.pace(START + HOUR), None);
    }

    #[test]
    fn inverted_window_does_not_produce_negative_pace() {
        let w = Window::new(40.0, Some(START), Some(START - HOUR));
        assert_eq!(w.pace(START), None);
    }

    #[test]
    fn fully_exhausted_quota_reports_pace_not_infinity() {
        let pace = window(100.0).pace(START + 5 * HOUR / 2).unwrap();
        assert!(pace.is_finite(), "pace must stay finite, got {pace}");
        assert!((pace - 2.0).abs() < 0.01);
    }

    #[test]
    fn elapsed_is_clamped_past_the_reset_time() {
        // A stale window whose reset is overdue must not read as < 100% elapsed.
        let w = window(100.0);
        assert_eq!(w.elapsed_fraction(START + 50 * HOUR), Some(1.0));
        assert!((w.pace(START + 50 * HOUR).unwrap() - 1.0).abs() < 0.01);
    }

    #[test]
    fn clock_behind_window_start_reports_zero_elapsed() {
        assert_eq!(window(0.0).elapsed_fraction(START - HOUR), Some(0.0));
    }

    #[test]
    fn resets_in_never_goes_negative() {
        assert_eq!(window(0.0).resets_in_ms(START + 50 * HOUR), Some(0));
        assert_eq!(window(0.0).resets_in_ms(START), Some(5 * HOUR));
    }

    #[test]
    fn durations_read_as_spaced_units() {
        assert_eq!(format_duration(2 * HOUR + 5 * 60_000), "2h 5m");
        assert_eq!(format_duration(45 * 60_000), "45m");
        assert_eq!(format_duration(30_000), "30s");
        assert_eq!(format_duration(0), "0s");
    }

    #[test]
    fn week_scale_durations_use_days_not_three_digit_hours() {
        assert_eq!(format_duration(2 * 24 * HOUR + 14 * HOUR), "2d 14h");
        assert_eq!(format_duration(7 * 24 * HOUR), "7d");
    }

    /// A zero component is dropped, never padded — `4h`, not `4h 00m`.
    #[test]
    fn a_zero_component_is_omitted_rather_than_padded() {
        assert_eq!(format_duration(4 * HOUR), "4h");
        assert_eq!(format_duration(24 * HOUR), "1d");
        assert_eq!(format_duration(24 * HOUR + 30 * 60_000), "1d");
        assert_eq!(format_duration(60_000), "1m");
    }

    /// Only the two largest units survive, so the countdown cannot grow a third
    /// column and push the weekly block out of alignment.
    #[test]
    fn no_duration_renders_more_than_two_units() {
        let cases = [
            59_000,
            60_000,
            HOUR - 1,
            HOUR + 59 * 60_000 + 59_000,
            25 * HOUR + 59 * 60_000,
            400 * 24 * HOUR + 23 * HOUR + 59 * 60_000,
        ];
        for ms in cases {
            let rendered = format_duration(ms);
            assert!(
                rendered.matches(' ').count() <= 1,
                "{ms}ms rendered as {rendered:?}, which is more than two units"
            );
        }
    }

    #[test]
    fn negative_durations_do_not_render_as_garbage() {
        assert_eq!(format_duration(-5_000), "0s");
    }

    #[test]
    fn a_window_with_no_end_has_no_reset_countdown() {
        assert_eq!(
            Window::new(10.0, Some(START), None).resets_in_ms(START),
            None
        );
    }

    #[test]
    fn an_end_without_a_start_gives_no_pace_but_still_counts_down() {
        let w = Window::new(10.0, None, Some(START + HOUR));
        assert_eq!(w.pace(START), None);
        assert_eq!(w.resets_in_ms(START), Some(HOUR));
    }

    #[test]
    fn elapsed_exactly_at_the_threshold_produces_a_signal() {
        // MIN_ELAPSED_FRACTION is a floor, not a gap: 2% of the window must
        // report rather than fall into the dead zone.
        let w = window(1.0);
        let at_threshold = START + (5.0 * HOUR as f64 * MIN_ELAPSED_FRACTION) as i64;
        assert!(w.pace(at_threshold).is_some());
    }

    #[test]
    fn duration_units_change_over_at_the_right_second() {
        assert_eq!(format_duration(59_000), "59s");
        assert_eq!(format_duration(60_000), "1m");
        assert_eq!(format_duration(3_599_000), "59m");
        assert_eq!(format_duration(HOUR), "1h");
        assert_eq!(format_duration(24 * HOUR - 1000), "23h 59m");
        assert_eq!(format_duration(24 * HOUR), "1d");
    }

    #[test]
    fn sub_second_durations_round_down_to_zero_rather_than_vanish() {
        assert_eq!(format_duration(999), "0s");
    }

    #[test]
    fn usage_above_one_hundred_percent_is_reported_not_clamped() {
        // If a provider ever reports over-quota, silently clamping would hide it.
        let pace = window(120.0).pace(START + 5 * HOUR / 2).unwrap();
        assert!(pace > 2.0, "got {pace}");
    }
}
