//! MiniMax Token Plan adapter.
//!
//! `GET https://api.minimax.io/v1/token_plan/remains`, `Authorization: Bearer <key>`.
//! `www.minimax.io` serves the identical document; `api.minimaxi.com` is the CN twin.
//!
//! Two things about this response are easy to get wrong:
//!
//! 1. `current_*_remaining_percent` is **remaining**, not consumed. It is inverted
//!    here so nothing downstream has to remember that.
//! 2. `remains_time` is a countdown in milliseconds to the end of the window — it
//!    is wall-clock time, not quota. It ticks down whether or not you make a
//!    single request, which is why it is ignored entirely; `end_time` says the
//!    same thing without inviting the misreading.
//!
//! `current_interval_usage_count` / `current_interval_total_count` are both `0` on
//! the `general` model for a live Max plan, so counts are unusable as a signal and
//! the percentages are the only real data here.

use crate::pace::Window;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::Usage;

/// The entry covering text/coding models. The response also carries `video` (and
/// potentially others) on their own independent quotas.
const CODING_MODEL: &str = "general";

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    model_remains: Vec<ModelRemain>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(Debug, Deserialize)]
struct BaseResp {
    #[serde(default)]
    status_code: i64,
    #[serde(default)]
    status_msg: String,
}

#[derive(Debug, Deserialize)]
struct ModelRemain {
    #[serde(default)]
    model_name: String,

    start_time: Option<i64>,
    end_time: Option<i64>,
    current_interval_remaining_percent: Option<f64>,

    weekly_start_time: Option<i64>,
    weekly_end_time: Option<i64>,
    current_weekly_remaining_percent: Option<f64>,
}

pub fn parse(body: &str) -> Result<Usage> {
    let response: Response = serde_json::from_str(body)
        .context("MiniMax returned a body that is not the expected JSON")?;

    if let Some(base) = &response.base_resp {
        if base.status_code != 0 {
            let msg = if base.status_msg.is_empty() {
                "no message"
            } else {
                &base.status_msg
            };
            // 1004 is overwhelmingly "wrong key type": a pay-as-you-go API key
            // where a Token Plan Subscription Key is required.
            let hint = if base.status_code == 1004 {
                " (1004 usually means this is a pay-as-you-go API key, not a Token Plan Subscription Key — take the key from Account / Token Plan)"
            } else {
                ""
            };
            return Err(anyhow!("MiniMax error {}: {msg}{hint}", base.status_code));
        }
    }

    let entry = response
        .model_remains
        .iter()
        .find(|m| m.model_name == CODING_MODEL)
        .or_else(|| response.model_remains.first())
        .ok_or_else(|| anyhow!("MiniMax returned no model_remains entries"))?;

    Ok(Usage::new(
        entry
            .current_interval_remaining_percent
            .map(|remaining| Window::new(100.0 - remaining, entry.start_time, entry.end_time)),
        entry.current_weekly_remaining_percent.map(|remaining| {
            Window::new(
                100.0 - remaining,
                entry.weekly_start_time,
                entry.weekly_end_time,
            )
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = include_str!("../../tests/fixtures/minimax_ok.json");

    #[test]
    fn parses_a_live_response_and_inverts_the_percentages() {
        let usage = parse(LIVE).unwrap();

        // Fixture reports 99% remaining on the 5h window and 51% on the week.
        let interval = usage.interval.unwrap();
        assert!((interval.used_percent - 1.0).abs() < f64::EPSILON);

        let weekly = usage.weekly.unwrap();
        assert!((weekly.used_percent - 49.0).abs() < f64::EPSILON);
    }

    #[test]
    fn picks_the_general_model_not_the_video_quota() {
        // The video entry sits at 100% remaining; picking it would mask real burn.
        let weekly = parse(LIVE).unwrap().weekly.unwrap();
        assert!(weekly.used_percent > 0.0, "picked the wrong model entry");
    }

    #[test]
    fn window_boundaries_survive_parsing() {
        let interval = parse(LIVE).unwrap().interval.unwrap();
        let span = interval.end_ms.unwrap() - interval.start_ms.unwrap();
        assert_eq!(span, 5 * 3_600_000, "expected a five-hour window");
    }

    #[test]
    fn wrong_key_type_is_reported_with_the_subscription_key_hint() {
        let body = r#"{"base_resp":{"status_code":1004,"status_msg":"login fail"}}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("1004"), "{err}");
        assert!(err.contains("Subscription Key"), "{err}");
    }

    #[test]
    fn a_nonzero_status_never_yields_a_usage_value() {
        let body = r#"{"model_remains":[{"model_name":"general","current_interval_remaining_percent":99}],
                       "base_resp":{"status_code":2049,"status_msg":"invalid api key"}}"#;
        assert!(
            parse(body).is_err(),
            "error bodies must not be parsed as data"
        );
    }

    #[test]
    fn an_empty_model_list_is_an_error_not_a_blank_row() {
        let body = r#"{"model_remains":[],"base_resp":{"status_code":0,"status_msg":"success"}}"#;
        assert!(parse(body).is_err());
    }

    #[test]
    fn missing_percentages_yield_no_window_rather_than_zero() {
        let body = r#"{"model_remains":[{"model_name":"general"}],
                       "base_resp":{"status_code":0,"status_msg":"success"}}"#;
        let usage = parse(body).unwrap();
        assert!(usage.interval.is_none());
        assert!(usage.weekly.is_none());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse("not json").is_err());
        assert!(parse("").is_err());
    }
}
