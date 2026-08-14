//! Z.ai / GLM Coding Plan adapter.
//!
//! `GET https://api.z.ai/api/monitor/usage/quota/limit`. The Authorization header
//! carries the raw token with **no `Bearer` prefix** — a `Bearer` prefix happens
//! to be accepted too, but the raw form is what Z.ai's own clients send.
//!
//! Unlike MiniMax, `percentage` here is **consumed**, not remaining. Two pieces of
//! evidence: an idle account reports `0` on the 5-hour window (as remaining that
//! would mean exhausted, yet requests still succeed), and Z.ai's own docs describe
//! the console figure as a percentage of usage. Falsify it by burning quota and
//! watching whether the number climbs — see README.
//!
//! Only `TOKENS_LIMIT` entries are read. The `TIME_LIMIT` entry is the monthly
//! MCP-tool allowance (web search, zread), which is a separate budget from the
//! coding quota.

use crate::pace::Window;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::Usage;

const TOKENS_LIMIT: &str = "TOKENS_LIMIT";

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<Data>,
}

#[derive(Debug, Deserialize)]
struct Data {
    #[serde(default)]
    limits: Vec<Limit>,
}

#[derive(Debug, Deserialize)]
struct Limit {
    #[serde(rename = "type", default)]
    kind: String,
    unit: Option<i64>,
    number: Option<i64>,
    percentage: Option<f64>,
    #[serde(rename = "nextResetTime")]
    next_reset_time: Option<i64>,
}

/// Window length from Z.ai's `unit` code and `number` multiplier.
///
/// Observed live: `unit: 3, number: 5` is the five-hour window and
/// `unit: 6, number: 1` is the week. Month (`5`) is deliberately unmapped — it
/// only ever appears on the MCP `TIME_LIMIT` entry, which this adapter skips, and
/// calendar months have no fixed length to divide by.
fn duration_ms(unit: i64, number: i64) -> Option<i64> {
    let base = match unit {
        3 => 3_600_000,   // hour
        4 => 86_400_000,  // day
        6 => 604_800_000, // week
        _ => return None,
    };
    number.checked_mul(base).filter(|ms| *ms > 0)
}

pub fn parse(body: &str) -> Result<Usage> {
    let response: Response =
        serde_json::from_str(body).context("Z.ai returned a body that is not the expected JSON")?;

    let ok = response.success.unwrap_or(true) && response.code.unwrap_or(200) == 200;
    if !ok {
        let msg = response.msg.unwrap_or_else(|| "no message".into());
        return Err(anyhow!(
            "Z.ai error {}: {msg}",
            response.code.unwrap_or_default()
        ));
    }

    let limits = response
        .data
        .ok_or_else(|| anyhow!("Z.ai response carried no data object"))?
        .limits;

    // Slot by window length rather than by array position, so a reordered or
    // extended response cannot silently swap the 5-hour and weekly figures.
    let mut windows: Vec<(Option<i64>, Window)> = limits
        .iter()
        .filter(|l| l.kind == TOKENS_LIMIT)
        .filter_map(|l| {
            let used = l.percentage?;
            let span = l.unit.zip(l.number).and_then(|(u, n)| duration_ms(u, n));
            // Z.ai gives only the reset time; the start is one window back from it.
            // With no reset time the window has not started, so there are no
            // boundaries and therefore no pace signal — usage still reports.
            let (start, end) = match (span, l.next_reset_time) {
                (Some(span), Some(reset)) => (Some(reset - span), Some(reset)),
                _ => (None, None),
            };
            Some((span, Window::new(used, start, end)))
        })
        .collect();

    if windows.is_empty() {
        return Err(anyhow!("Z.ai returned no {TOKENS_LIMIT} entries"));
    }

    // Unknown spans sort last so a recognized window is never displaced by one
    // this adapter could not measure.
    windows.sort_by_key(|(span, _)| span.unwrap_or(i64::MAX));

    let mut iter = windows.into_iter().map(|(_, w)| w);
    let interval = iter.next();
    let weekly = iter.next_back();

    Ok(Usage::new(interval, weekly))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = include_str!("../../tests/fixtures/zai_ok.json");

    #[test]
    fn parses_a_live_response_treating_percentage_as_consumed() {
        let usage = parse(LIVE).unwrap();
        assert_eq!(usage.interval.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(usage.weekly.as_ref().unwrap().used_percent, 20.0);
    }

    #[test]
    fn the_weekly_window_spans_exactly_seven_days() {
        let weekly = parse(LIVE).unwrap().weekly.unwrap();
        let span = weekly.end_ms.unwrap() - weekly.start_ms.unwrap();
        assert_eq!(span, 604_800_000);
    }

    #[test]
    fn a_window_with_no_reset_time_reports_usage_but_no_pace() {
        // The live 5h entry is idle and carries no nextResetTime.
        let interval = parse(LIVE).unwrap().interval.unwrap();
        assert!(interval.start_ms.is_none());
        assert_eq!(interval.pace(1_786_701_600_000), None);
    }

    #[test]
    fn the_monthly_mcp_tool_budget_is_not_mistaken_for_coding_quota() {
        // TIME_LIMIT sits at 0% while the week is at 20%; if it leaked in as the
        // weekly window the burn signal would read as idle.
        assert_eq!(parse(LIVE).unwrap().weekly.unwrap().used_percent, 20.0);
    }

    #[test]
    fn windows_are_slotted_by_length_not_array_order() {
        let reordered = r#"{"code":200,"success":true,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":20,"nextResetTime":1787097212998},
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":7,"nextResetTime":1786719600000}
        ]}}"#;
        let usage = parse(reordered).unwrap();
        assert_eq!(usage.interval.unwrap().used_percent, 7.0);
        assert_eq!(usage.weekly.unwrap().used_percent, 20.0);
    }

    #[test]
    fn a_failed_response_never_yields_usage() {
        let body = r#"{"code":401,"msg":"unauthorized","success":false}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("401"), "{err}");
    }

    #[test]
    fn an_empty_limits_list_is_an_error_not_a_blank_row() {
        let body = r#"{"code":200,"success":true,"data":{"limits":[]}}"#;
        assert!(parse(body).is_err());
    }

    #[test]
    fn an_unknown_unit_code_does_not_fabricate_a_window() {
        let body = r#"{"code":200,"success":true,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":99,"number":1,"percentage":30,"nextResetTime":1787097212998}
        ]}}"#;
        let interval = parse(body).unwrap().interval.unwrap();
        assert_eq!(interval.used_percent, 30.0);
        assert!(
            interval.start_ms.is_none(),
            "unmeasurable span must not become a window"
        );
    }

    #[test]
    fn a_single_window_fills_interval_and_leaves_weekly_empty() {
        let body = r#"{"code":200,"success":true,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":12,"nextResetTime":1786719600000}
        ]}}"#;
        let usage = parse(body).unwrap();
        assert_eq!(usage.interval.unwrap().used_percent, 12.0);
        assert!(usage.weekly.is_none());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse("not json").is_err());
        assert!(parse("").is_err());
    }
}
