//! llm-watcher — report coding-plan quota burn rate across providers and accounts.
//!
//! Runs, prints, exits. No daemon, no database, no listening port, and nothing
//! written into `~/.claude/`.

mod config;
mod pace;
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
    let reports: Vec<Report> = accounts
        .iter()
        .map(|account| {
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

    if args.json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_table(&reports);
    }

    if reports.iter().all(|r| r.error.is_some()) {
        return Ok(std::process::ExitCode::from(2));
    }
    if let Some(threshold) = args.threshold {
        let breached = reports.iter().any(|r| {
            [&r.interval, &r.weekly]
                .into_iter()
                .flatten()
                .any(|w| w.pace.is_some_and(|p| p >= threshold))
        });
        if breached {
            return Ok(std::process::ExitCode::from(1));
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
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

fn colorize_pace(pace: f64, text: &str) -> String {
    if pace >= 1.5 {
        text.red().bold().to_string()
    } else if pace >= 1.0 {
        text.yellow().to_string()
    } else {
        text.green().to_string()
    }
}
