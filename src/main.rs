//! llm-watcher — report coding-plan quota burn rate across providers and accounts.
//!
//! Runs, prints, exits. No daemon, no database, no listening port, and nothing
//! written into `~/.claude/`.

mod config;
mod pace;
mod progress;
mod provider;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use pace::{format_duration, Window};
use provider::Usage;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(
    name = "llm-watcher",
    version,
    about = "Report LLM coding-plan quota burn rate",
    long_about = "Reports how fast each configured coding plan is consuming its quota.\n\n\
                  Pace is the unit-free ratio of quota consumed to window elapsed:\n\
                  1.0x lands exactly at reset, above 1.0x exhausts early, below leaves headroom."
)]
struct Args {
    /// Only report this account (repeatable)
    #[arg(short, long, value_name = "NAME")]
    account: Vec<String>,

    /// Emit JSON instead of a table
    #[arg(long)]
    json: bool,

    /// Exit non-zero if any window's pace reaches this ratio
    #[arg(short, long, value_name = "RATIO")]
    threshold: Option<f64>,

    /// Never colorize output
    #[arg(long)]
    no_color: bool,
}

#[derive(Serialize)]
struct Report {
    name: String,
    provider: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval: Option<WindowReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekly: Option<WindowReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct WindowReport {
    used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pace: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_in_ms: Option<i64>,
}

impl WindowReport {
    fn from(window: &Window, now_ms: i64) -> Self {
        Self {
            used_percent: window.used_percent,
            pace: window.pace(now_ms),
            resets_in_ms: window.resets_in_ms(now_ms),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", "error:".red().bold());
            std::process::ExitCode::from(2)
        }
    }
}

fn run() -> Result<std::process::ExitCode> {
    let args = Args::parse();
    if args.no_color {
        colored::control::set_override(false);
    }

    let mut accounts = config::load()?;
    if !args.account.is_empty() {
        accounts.retain(|a| args.account.iter().any(|want| want == &a.name));
        if accounts.is_empty() {
            anyhow::bail!("no configured account matched {:?}", args.account);
        }
    }
    if accounts.is_empty() {
        anyhow::bail!(
            "no accounts configured — set MINIMAX_API_KEY / ZHIPU_API_KEY, or write {}",
            config::config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the config file".into())
        );
    }

    let now = now_ms();
    // Accounts are queried one at a time against a generous timeout, so without
    // this the tool is indistinguishable from a hang.
    let mut spinner = progress::Spinner::new(accounts.len());
    let reports: Vec<Report> = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            spinner.set(index + 1, &account.name);
            // One bad key must not hide the other plans, so failures land on the
            // row instead of aborting the run.
            let outcome = account.key().and_then(|key| account.provider.fetch(&key));
            match outcome {
                Ok(Usage { interval, weekly }) => Report {
                    name: account.name.clone(),
                    provider: account.provider.as_str(),
                    interval: interval.as_ref().map(|w| WindowReport::from(w, now)),
                    weekly: weekly.as_ref().map(|w| WindowReport::from(w, now)),
                    error: None,
                },
                Err(e) => Report {
                    name: account.name.clone(),
                    provider: account.provider.as_str(),
                    interval: None,
                    weekly: None,
                    error: Some(format!("{e:#}")),
                },
            }
        })
        .collect();
    // Erase the status line before anything lands on stdout, so the two streams
    // cannot interleave on a shared terminal.
    spinner.finish();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_table(&reports);
    }

    Ok(std::process::ExitCode::from(outcome_code(
        &reports,
        args.threshold,
    )))
}

/// Exit status: `2` when nothing could be read at all, `1` when a threshold was
/// breached, `0` otherwise.
///
/// A partial failure stays `0`/`1` — one dead key must not mask a real pace
/// signal from the accounts that did answer.
fn outcome_code(reports: &[Report], threshold: Option<f64>) -> u8 {
    if !reports.is_empty() && reports.iter().all(|r| r.error.is_some()) {
        return 2;
    }
    if let Some(threshold) = threshold {
        let breached = reports.iter().any(|r| {
            [&r.interval, &r.weekly]
                .into_iter()
                .flatten()
                .any(|w| w.pace.is_some_and(|p| p >= threshold))
        });
        if breached {
            return 1;
        }
    }
    0
}

fn print_table(reports: &[Report]) {
    let name_width = reports
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);

    // Cells are rendered up front so the 5-hour column can be sized to its widest
    // entry. A fixed width would either clip a long row or leave the short ones
    // stranded, and neither is visible until a window happens to be near reset.
    let cells: Vec<(&Report, (String, usize))> = reports
        .iter()
        .map(|r| (r, render_window(r.interval.as_ref())))
        .collect();

    let interval_width = cells
        .iter()
        .filter(|(r, _)| r.error.is_none())
        .map(|(_, (_, w))| *w)
        .max()
        .unwrap_or(0)
        .max("5H WINDOW".len());

    println!(
        "{:<name_width$}  {}{:pad$}  {}",
        "PLAN".bold(),
        "5H WINDOW".bold(),
        "",
        "WEEKLY".bold(),
        pad = interval_width - "5H WINDOW".len()
    );

    for (report, (interval, width)) in cells {
        if let Some(error) = &report.error {
            println!(
                "{:<name_width$}  {}",
                report.name,
                format!("({error})").red()
            );
            continue;
        }
        println!(
            "{:<name_width$}  {interval}{:pad$}  {}",
            report.name,
            "",
            render_window(report.weekly.as_ref()).0,
            pad = interval_width.saturating_sub(width)
        );
    }
}

/// One window as `49% used   1.2x  resets 2d14h`, plus its *visible* width.
///
/// The width is returned separately because colouring embeds ANSI escapes, and a
/// `{:<26}` format spec counts those bytes — so coloured columns would drift out
/// of alignment while uncoloured ones looked fine.
fn render_window(window: Option<&WindowReport>) -> (String, usize) {
    let Some(window) = window else {
        return ("-".to_string(), 1);
    };

    let pace_text = match window.pace {
        Some(p) => format!("{p:>5.1}x"),
        // A window that has just reset has no honest pace to report yet.
        None => format!("{:>6}", "--"),
    };
    let pace = match window.pace {
        Some(p) => colorize_pace(p, &pace_text),
        None => pace_text.dimmed().to_string(),
    };
    let resets = window
        .resets_in_ms
        .map(|ms| format!("resets {}", format_duration(ms)))
        .unwrap_or_default();

    let used = format!("{:>3.0}% used ", window.used_percent);
    let width = used.len() + pace_text.len() + 2 + resets.len();
    (format!("{used}{pace}  {resets}"), width)
}

/// Severity bands for a pace ratio, split from the colouring so the thresholds
/// can be checked without depending on which escape codes `colored` emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaceLevel {
    /// Below 1.0x — the window will not run out before it resets.
    Headroom,
    /// 1.0x..1.5x — on track to exhaust at or shortly before reset.
    Tight,
    /// 1.5x and above.
    Overrunning,
}

fn pace_level(pace: f64) -> PaceLevel {
    // NaN fails every comparison and lands in `Headroom`. That is the right way
    // round: a number we cannot interpret must not raise a false alarm.
    if pace >= 1.5 {
        PaceLevel::Overrunning
    } else if pace >= 1.0 {
        PaceLevel::Tight
    } else {
        PaceLevel::Headroom
    }
}

fn colorize_pace(pace: f64, text: &str) -> String {
    match pace_level(pace) {
        PaceLevel::Overrunning => text.red().bold().to_string(),
        PaceLevel::Tight => text.yellow().to_string(),
        PaceLevel::Headroom => text.green().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible width of a cell, ignoring any ANSI escapes `colored` added.
    /// `render_window` promises exactly this number, and the table's alignment
    /// is only correct while the promise holds.
    fn visible_len(s: &str) -> usize {
        let mut width = 0;
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip through the terminating letter of the CSI sequence.
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                width += 1;
            }
        }
        width
    }

    fn window(used: f64, pace: Option<f64>, resets_in_ms: Option<i64>) -> WindowReport {
        WindowReport {
            used_percent: used,
            pace,
            resets_in_ms,
        }
    }

    fn report(name: &str, interval: Option<WindowReport>, error: Option<&str>) -> Report {
        Report {
            name: name.to_string(),
            provider: "minimax",
            interval,
            weekly: None,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_missing_window_renders_as_a_dash() {
        assert_eq!(render_window(None), ("-".to_string(), 1));
    }

    #[test]
    fn the_reported_width_matches_what_the_terminal_actually_shows() {
        // The bug this guards: colour escapes counted as visible characters,
        // which pushes coloured columns out of alignment while plain ones look
        // fine. Force colour on so the escapes are actually present.
        colored::control::set_override(true);
        for pace in [Some(0.4), Some(1.2), Some(3.0), None] {
            let (text, width) = render_window(Some(&window(49.0, pace, Some(9_000_000))));
            assert_eq!(
                visible_len(&text),
                width,
                "width {width} disagrees with {text:?}"
            );
        }
        colored::control::unset_override();
    }

    #[test]
    fn width_is_stable_across_the_colour_setting() {
        let w = window(49.0, Some(2.0), Some(9_000_000));
        colored::control::set_override(true);
        let colored_width = render_window(Some(&w)).1;
        colored::control::set_override(false);
        let plain = render_window(Some(&w));
        colored::control::unset_override();

        assert_eq!(colored_width, plain.1);
        assert_eq!(visible_len(&plain.0), plain.1);
        assert!(!plain.0.contains('\x1b'), "no_color must suppress escapes");
    }

    #[test]
    fn a_window_with_no_pace_yet_shows_a_placeholder_not_a_number() {
        colored::control::set_override(false);
        let (text, _) = render_window(Some(&window(3.0, None, Some(9_000_000))));
        colored::control::unset_override();
        assert!(text.contains("--"), "{text}");
        assert!(text.contains("3% used"), "{text}");
    }

    #[test]
    fn a_window_with_no_reset_time_omits_the_resets_clause() {
        colored::control::set_override(false);
        let (text, width) = render_window(Some(&window(10.0, Some(0.5), None)));
        colored::control::unset_override();
        assert!(!text.contains("resets"), "{text}");
        assert_eq!(visible_len(&text), width);
    }

    #[test]
    fn pace_thresholds_split_at_one_and_one_and_a_half() {
        assert_eq!(pace_level(0.0), PaceLevel::Headroom);
        assert_eq!(pace_level(0.999), PaceLevel::Headroom);
        assert_eq!(pace_level(1.0), PaceLevel::Tight, "1.0x is on the edge");
        assert_eq!(pace_level(1.499), PaceLevel::Tight);
        assert_eq!(pace_level(1.5), PaceLevel::Overrunning);
        assert_eq!(pace_level(100.0), PaceLevel::Overrunning);
    }

    #[test]
    fn an_uninterpretable_pace_does_not_raise_a_false_alarm() {
        assert_eq!(pace_level(f64::NAN), PaceLevel::Headroom);
        assert_eq!(pace_level(f64::INFINITY), PaceLevel::Overrunning);
        assert_eq!(pace_level(f64::NEG_INFINITY), PaceLevel::Headroom);
    }

    #[test]
    fn colouring_never_drops_the_text_it_was_given() {
        colored::control::set_override(true);
        for pace in [0.1, 1.0, 2.0] {
            assert!(colorize_pace(pace, "  1.2x").contains("1.2x"));
        }
        colored::control::unset_override();
    }

    #[test]
    fn a_window_report_carries_pace_and_reset_across_from_the_window() {
        const START: i64 = 1_786_701_600_000;
        const HOUR: i64 = 3_600_000;
        let w = Window::new(50.0, Some(START), Some(START + 4 * HOUR));
        let report = WindowReport::from(&w, START + 2 * HOUR);
        assert_eq!(report.used_percent, 50.0);
        assert!((report.pace.unwrap() - 1.0).abs() < 0.01);
        assert_eq!(report.resets_in_ms, Some(2 * HOUR));
    }

    #[test]
    fn a_clean_run_exits_zero() {
        let reports = vec![report("a", Some(window(10.0, Some(0.5), None)), None)];
        assert_eq!(outcome_code(&reports, None), 0);
    }

    #[test]
    fn every_account_failing_exits_two() {
        let reports = vec![
            report("a", None, Some("no key")),
            report("b", None, Some("401")),
        ];
        assert_eq!(outcome_code(&reports, None), 2);
    }

    #[test]
    fn one_dead_key_does_not_mask_the_accounts_that_answered() {
        let reports = vec![
            report("dead", None, Some("no key")),
            report("live", Some(window(10.0, Some(0.5), None)), None),
        ];
        assert_eq!(outcome_code(&reports, None), 0);
        // ...and the live account can still trip the threshold on its own.
        let hot = vec![
            report("dead", None, Some("no key")),
            report("live", Some(window(90.0, Some(2.0), None)), None),
        ];
        assert_eq!(outcome_code(&hot, Some(1.5)), 1);
    }

    #[test]
    fn the_threshold_is_inclusive() {
        let reports = vec![report("a", Some(window(50.0, Some(1.5), None)), None)];
        assert_eq!(outcome_code(&reports, Some(1.5)), 1);
        assert_eq!(outcome_code(&reports, Some(1.6)), 0);
    }

    #[test]
    fn a_window_with_no_pace_can_never_breach_a_threshold() {
        // A freshly reset window reports no pace; treating that as a breach
        // would fire an alarm every time a window rolls over.
        let reports = vec![report("a", Some(window(99.0, None, None)), None)];
        assert_eq!(outcome_code(&reports, Some(0.1)), 0);
    }

    #[test]
    fn the_weekly_window_can_breach_on_its_own() {
        let reports = vec![Report {
            name: "a".into(),
            provider: "zai",
            interval: Some(window(1.0, Some(0.1), None)),
            weekly: Some(window(80.0, Some(2.4), None)),
            error: None,
        }];
        assert_eq!(outcome_code(&reports, Some(2.0)), 1);
    }

    #[test]
    fn a_failure_outranks_a_threshold_breach() {
        let reports = vec![report("a", None, Some("401"))];
        assert_eq!(outcome_code(&reports, Some(0.1)), 2);
    }

    #[test]
    fn an_empty_report_set_is_not_treated_as_total_failure() {
        assert_eq!(outcome_code(&[], None), 0);
        assert_eq!(outcome_code(&[], Some(1.0)), 0);
    }

    #[test]
    fn json_omits_absent_windows_rather_than_emitting_null() {
        let reports = vec![report("a", None, Some("no key"))];
        let json = serde_json::to_string(&reports).unwrap();
        assert!(json.contains("\"error\""), "{json}");
        assert!(!json.contains("\"interval\""), "{json}");
        assert!(!json.contains("null"), "{json}");
    }

    #[test]
    fn a_healthy_row_serializes_without_an_error_field() {
        let reports = vec![report(
            "a",
            Some(window(10.0, Some(0.5), Some(60_000))),
            None,
        )];
        let json = serde_json::to_string(&reports).unwrap();
        assert!(!json.contains("\"error\""), "{json}");
        assert!(json.contains("\"used_percent\":10.0"), "{json}");
    }

    #[test]
    fn printing_a_table_of_only_failures_does_not_panic() {
        // `interval_width` is derived from the non-error rows; with none left
        // the column width has to fall back rather than underflow.
        colored::control::set_override(false);
        print_table(&[report("a", None, Some("no key"))]);
        print_table(&[]);
        colored::control::unset_override();
    }

    #[test]
    fn printing_a_table_with_a_name_shorter_than_the_header_does_not_panic() {
        colored::control::set_override(false);
        print_table(&[report("x", Some(window(1.0, Some(0.1), Some(1_000))), None)]);
        colored::control::unset_override();
    }
}
