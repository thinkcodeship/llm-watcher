//! Anthropic Claude subscription adapter (Pro, Max 5x, Max 20x).
//!
//! `GET https://api.anthropic.com/api/oauth/usage`, `Authorization: Bearer <OAuth
//! access token>`. No `anthropic-beta` header is needed, and no `x-api-key`: this
//! is the subscription surface, not the pay-as-you-go API, and a Max plan has no
//! API key at all. See [`crate::credentials`] for where the token comes from.
//!
//! Three things about this response are easy to get wrong:
//!
//! 1. `percent` / `utilization` is **consumed**, the same direction as Z.ai and
//!    the opposite of MiniMax. No inversion here.
//! 2. Only the *reset* time is given, never the window start. The start is one
//!    span back from the reset, and the span comes from which window it is —
//!    5 hours for the session, 7 days for the weekly ones.
//! 3. An unauthenticated or stale token answers **429**, not 401. Status code
//!    alone therefore cannot distinguish "rate limited" from "log in again",
//!    which is why an expired token is caught before the request is ever sent.
//!
//! The `limits` array is preferred over the older `five_hour` / `seven_day`
//! scalars because it also carries per-model caps. Entries are keyed on `group`
//! plus the presence of `scope` rather than on `kind`, so a new `kind` string
//! cannot silently drop a window.
//!
//! Fields named after unreleased features (`tangelo`, `nimbus_quill`,
//! `cinder_cove`, `iguana_necktie`, `amber_ladder`, `omelette*`) are deliberately
//! not parsed. They are flags that churn, and guessing at one would invent a
//! window that does not exist.

use crate::pace::Window;
use crate::rfc3339;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::{ScopedWindow, Usage};

/// The session window is five hours on every current plan.
const SESSION_MS: i64 = 5 * 3_600_000;
/// Both weekly windows run seven days.
const WEEKLY_MS: i64 = 7 * 86_400_000;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    error: Option<ApiError>,
    #[serde(default)]
    limits: Vec<Limit>,
    #[serde(default)]
    five_hour: Option<Scalar>,
    #[serde(default)]
    seven_day: Option<Scalar>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: String,
}

/// The older top-level shape, still sent alongside `limits`.
#[derive(Debug, Deserialize)]
struct Scalar {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Limit {
    #[serde(default)]
    group: String,
    percent: Option<f64>,
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<Scope>,
}

#[derive(Debug, Deserialize)]
struct Scope {
    #[serde(default)]
    model: Option<Model>,
}

#[derive(Debug, Deserialize)]
struct Model {
    #[serde(default)]
    display_name: Option<String>,
}

/// Build a window from a consumed percentage and a reset time.
///
/// Anthropic reports only the reset, so the start is one span back from it. With
/// no reset time the window has no boundaries and therefore no pace signal —
/// usage still reports.
fn window(used_percent: f64, resets_at: Option<&str>, span_ms: i64) -> Window {
    let end = resets_at.and_then(rfc3339::to_epoch_ms);
    match end {
        Some(end) => Window::new(used_percent, Some(end - span_ms), Some(end)),
        None => Window::new(used_percent, None, None),
    }
}

pub fn parse(body: &str) -> Result<Usage> {
    let response: Response = serde_json::from_str(body)
        .context("Anthropic returned a body that is not the expected JSON")?;

    if let Some(error) = response.error {
        let kind = if error.kind.is_empty() {
            "error".to_string()
        } else {
            error.kind
        };
        let message = if error.message.is_empty() {
            "no message".to_string()
        } else {
            error.message
        };
        return Err(anyhow!("Anthropic {kind}: {message}"));
    }

    let (mut interval, mut weekly, mut scoped) = (None, None, Vec::new());

    // Slot on `group` plus the presence of `scope`. Keying on `kind` would drop
    // a window the moment Anthropic adds a new one, and keying on `group` alone
    // would let the per-model cap overwrite the all-model weekly.
    for limit in &response.limits {
        let Some(percent) = limit.percent else {
            continue;
        };
        let resets_at = limit.resets_at.as_deref();

        let model = limit
            .scope
            .as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.clone());

        match (limit.group.as_str(), model) {
            (_, Some(label)) => scoped.push(ScopedWindow {
                label,
                window: window(percent, resets_at, WEEKLY_MS),
            }),
            ("session", None) => interval = Some(window(percent, resets_at, SESSION_MS)),
            ("weekly", None) => weekly = Some(window(percent, resets_at, WEEKLY_MS)),
            _ => {}
        }
    }

    // Fall back to the scalar fields for responses that predate `limits`.
    if interval.is_none() {
        interval = response
            .five_hour
            .as_ref()
            .and_then(|s| s.utilization)
            .map(|used| {
                window(
                    used,
                    response
                        .five_hour
                        .as_ref()
                        .and_then(|s| s.resets_at.as_deref()),
                    SESSION_MS,
                )
            });
    }
    if weekly.is_none() {
        weekly = response
            .seven_day
            .as_ref()
            .and_then(|s| s.utilization)
            .map(|used| {
                window(
                    used,
                    response
                        .seven_day
                        .as_ref()
                        .and_then(|s| s.resets_at.as_deref()),
                    WEEKLY_MS,
                )
            });
    }

    if interval.is_none() && weekly.is_none() && scoped.is_empty() {
        return Err(anyhow!("Anthropic reported no quota windows"));
    }

    Ok(Usage {
        interval,
        weekly,
        scoped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = include_str!("../../tests/fixtures/anthropic_ok.json");

    #[test]
    fn parses_a_live_response_treating_percent_as_consumed() {
        let usage = parse(LIVE).unwrap();
        assert_eq!(usage.interval.as_ref().unwrap().used_percent, 4.0);
        assert_eq!(usage.weekly.as_ref().unwrap().used_percent, 78.0);
    }

    #[test]
    fn the_session_window_spans_exactly_five_hours() {
        let interval = parse(LIVE).unwrap().interval.unwrap();
        let span = interval.end_ms.unwrap() - interval.start_ms.unwrap();
        assert_eq!(span, 5 * 3_600_000);
    }

    #[test]
    fn the_weekly_window_spans_exactly_seven_days() {
        let weekly = parse(LIVE).unwrap().weekly.unwrap();
        let span = weekly.end_ms.unwrap() - weekly.start_ms.unwrap();
        assert_eq!(span, 604_800_000);
    }

    #[test]
    fn the_per_model_weekly_cap_is_reported_with_its_model_name() {
        // On a 20x Max plan this frequently binds before the all-model weekly,
        // so losing it would hide the limit that actually stops work.
        let scoped = parse(LIVE).unwrap().scoped;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].label, "Fable");
        assert_eq!(scoped[0].window.used_percent, 61.0);
    }

    #[test]
    fn the_scoped_cap_is_not_mistaken_for_the_all_model_weekly() {
        // Both are group "weekly"; keying on group alone would let the 61%
        // scoped entry overwrite the 78% figure that drives the weekly row.
        let usage = parse(LIVE).unwrap();
        assert_eq!(usage.weekly.unwrap().used_percent, 78.0);
    }

    #[test]
    fn the_reset_time_is_converted_from_rfc3339_to_epoch_millis() {
        let interval = parse(LIVE).unwrap().interval.unwrap();
        assert_eq!(interval.end_ms, Some(1_786_718_400_255));
    }

    #[test]
    fn windows_are_slotted_by_group_not_array_order() {
        let reordered = r#"{"limits":[
            {"kind":"weekly_all","group":"weekly","percent":78,"resets_at":"2026-08-17T04:00:00Z","scope":null},
            {"kind":"session","group":"session","percent":4,"resets_at":"2026-08-14T14:40:00Z","scope":null}
        ]}"#;
        let usage = parse(reordered).unwrap();
        assert_eq!(usage.interval.unwrap().used_percent, 4.0);
        assert_eq!(usage.weekly.unwrap().used_percent, 78.0);
    }

    #[test]
    fn an_unknown_kind_within_a_known_group_is_still_reported() {
        // Keying on `kind` would drop this; keying on `group` keeps it.
        let body = r#"{"limits":[
            {"kind":"weekly_something_new","group":"weekly","percent":42,"resets_at":"2026-08-17T04:00:00Z","scope":null}
        ]}"#;
        assert_eq!(parse(body).unwrap().weekly.unwrap().used_percent, 42.0);
    }

    #[test]
    fn the_scalar_fields_are_used_when_the_limits_array_is_absent() {
        // Older responses carry only five_hour/seven_day.
        let body = r#"{"five_hour":{"utilization":9,"resets_at":"2026-08-14T14:40:00Z"},
                       "seven_day":{"utilization":33,"resets_at":"2026-08-17T04:00:00Z"}}"#;
        let usage = parse(body).unwrap();
        assert_eq!(usage.interval.unwrap().used_percent, 9.0);
        assert_eq!(usage.weekly.unwrap().used_percent, 33.0);
    }

    #[test]
    fn a_window_with_no_reset_time_reports_usage_but_no_pace() {
        let body = r#"{"five_hour":{"utilization":12,"resets_at":null}}"#;
        let interval = parse(body).unwrap().interval.unwrap();
        assert_eq!(interval.used_percent, 12.0);
        assert!(interval.start_ms.is_none());
        assert_eq!(interval.pace(1_786_718_400_000), None);
    }

    #[test]
    fn the_unreleased_codename_windows_are_not_parsed_as_quota() {
        // nimbus_quill sits at 0% in the fixture; leaking it in as a window
        // would report an idle burn rate that no plan actually has.
        let usage = parse(LIVE).unwrap();
        assert_eq!(usage.scoped.len(), 1, "only the model-scoped cap belongs");
        assert_eq!(usage.interval.unwrap().used_percent, 4.0);
    }

    #[test]
    fn an_error_envelope_never_yields_usage() {
        let body = r#"{"error":{"type":"rate_limit_error","message":"Rate limited. Please try again later."}}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("rate_limit_error"), "{err}");
        assert!(err.contains("Rate limited"), "{err}");
    }

    #[test]
    fn an_authentication_error_is_reported_with_its_message() {
        let body = r#"{"error":{"type":"authentication_error","message":"invalid bearer token"}}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("invalid bearer token"), "{err}");
    }

    #[test]
    fn a_response_with_no_windows_at_all_is_an_error_not_a_blank_row() {
        assert!(parse(r#"{"limits":[]}"#).is_err());
        assert!(parse("{}").is_err());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse("not json").is_err());
        assert!(parse("").is_err());
    }
}
