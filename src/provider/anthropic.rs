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

/// The reset time as epoch milliseconds.
///
/// Absent is absent. Present but unreadable is a shape change, not an absent
/// window: both used to collapse to the same boundary-less window, which renders
/// as the very `--` a healthy freshly-reset window shows, so a changed timestamp
/// format would have read as normal operation indefinitely.
fn reset_ms(resets_at: Option<&str>) -> Result<Option<i64>> {
    match resets_at {
        None => Ok(None),
        Some(text) => rfc3339::to_epoch_ms(text)
            .map(Some)
            .ok_or_else(|| anyhow!("Anthropic sent an unreadable reset time {text:?}")),
    }
}

/// Build a window from a consumed percentage and a reset time.
///
/// Anthropic reports only the reset, so the start is one span back from it. With
/// no reset time the window has no boundaries and therefore no pace signal —
/// usage still reports.
fn window(used_percent: f64, resets_at: Option<&str>, span_ms: i64) -> Result<Window> {
    let end = reset_ms(resets_at)?;
    Ok(Window::new(used_percent, end.map(|end| end - span_ms), end))
}

/// A per-model cap, whose group may not have a span this adapter knows.
///
/// An unknown span means no *start*, and therefore no pace. It does not mean no
/// *end*: the reset time was readable, and [`Window`] counts down from an end
/// with no start — see `an_end_without_a_start_gives_no_pace_but_still_counts_down`
/// in [`crate::pace`]. Dropping it would print strictly less than the response
/// gave us, and would skip validating a timestamp that is right there.
fn scoped_window(
    used_percent: f64,
    resets_at: Option<&str>,
    span_ms: Option<i64>,
) -> Result<Window> {
    match span_ms {
        Some(span) => window(used_percent, resets_at, span),
        None => Ok(Window::new(used_percent, None, reset_ms(resets_at)?)),
    }
}

/// Keep a window, or remember why it could not be built.
///
/// One unreadable reset time must not delete the windows that parsed cleanly —
/// that is the rule `main` applies across accounts ("One bad key must not hide
/// the other plans"), applied to the windows within one account. The remembered
/// reason becomes the error only when nothing survived, which is the shape a
/// real format change takes anyway.
fn take(built: Option<Result<Window>>, unreadable: &mut Option<String>) -> Option<Window> {
    match built? {
        Ok(window) => Some(window),
        Err(e) => {
            unreadable.get_or_insert_with(|| e.to_string());
            None
        }
    }
}

/// A window from the pre-`limits` scalar shape, when it carries a utilization.
fn scalar_window(scalar: Option<&Scalar>, span_ms: i64) -> Result<Option<Window>> {
    let Some(scalar) = scalar else {
        return Ok(None);
    };
    let Some(used) = scalar.utilization else {
        return Ok(None);
    };
    Ok(Some(window(used, scalar.resets_at.as_deref(), span_ms)?))
}

/// Window length for a limit group.
///
/// An unrecognized group has no known span, so it yields `None` rather than a
/// borrowed one: usage still reports, but no pace is fabricated from a guessed
/// length. Same rule as [`super::zai`]'s unknown unit code.
fn span_for(group: &str) -> Option<i64> {
    match group {
        "session" => Some(SESSION_MS),
        "weekly" => Some(WEEKLY_MS),
        _ => None,
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
    // Collected rather than propagated: see `take`.
    let mut unreadable: Option<String> = None;

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
            // Keyed on the group like every other arm: a model-scoped session
            // cap runs five hours, not the seven days a scoped entry used to be
            // assumed to have.
            (group, Some(label)) => {
                let built = scoped_window(percent, resets_at, span_for(group));
                if let Some(window) = take(Some(built), &mut unreadable) {
                    scoped.push(ScopedWindow { label, window });
                }
            }
            // Guarded like the scoped arm above: `take` returning None means
            // "nothing to add", never "clear what is already there". Assigning
            // unconditionally would let a second, malformed entry in the same
            // group erase a window that had already parsed cleanly — the exact
            // opposite of the rule `take` documents.
            ("session", None) => {
                if let Some(built) = take(
                    Some(window(percent, resets_at, SESSION_MS)),
                    &mut unreadable,
                ) {
                    interval = Some(built);
                }
            }
            ("weekly", None) => {
                if let Some(built) =
                    take(Some(window(percent, resets_at, WEEKLY_MS)), &mut unreadable)
                {
                    weekly = Some(built);
                }
            }
            _ => {}
        }
    }

    // Fall back to the scalar fields for responses that predate `limits`.
    if interval.is_none() {
        let built = scalar_window(response.five_hour.as_ref(), SESSION_MS).transpose();
        interval = take(built, &mut unreadable);
    }
    if weekly.is_none() {
        let built = scalar_window(response.seven_day.as_ref(), WEEKLY_MS).transpose();
        weekly = take(built, &mut unreadable);
    }

    if interval.is_none() && weekly.is_none() && scoped.is_empty() {
        // Nothing survived. If a timestamp was the reason, name it — that is the
        // diagnosis a format change needs, and it is the shape such a change
        // takes, since it fails every window at once.
        return Err(match unreadable {
            Some(reason) => anyhow!(reason),
            None => anyhow!("Anthropic reported no quota windows"),
        });
    }

    Ok(Usage {
        interval,
        weekly,
        scoped,
        // Something survived, so this is not fatal — but the window that did
        // not survive renders as the same `-` an absent one does, and silence
        // here would leave a format change looking like normal operation. That
        // is the blind spot the whole-account error above exists to close.
        degraded: unreadable,
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
    fn a_model_scoped_session_cap_is_measured_over_five_hours_not_seven_days() {
        // Stretching a 5-hour cap across a 7-day span divides its pace by ~33,
        // so a cap burning at 3x prints well under 1x and never trips
        // --threshold. A wrong number is worse than no number.
        let body = r#"{"limits":[
            {"kind":"session_scoped","group":"session","percent":50,
             "resets_at":"2026-08-14T14:40:00Z",
             "scope":{"model":{"display_name":"Fable"}}}
        ]}"#;
        let scoped = parse(body).unwrap().scoped;
        assert_eq!(scoped.len(), 1);
        let window = &scoped[0].window;
        assert_eq!(
            window.end_ms.unwrap() - window.start_ms.unwrap(),
            5 * 3_600_000
        );
    }

    #[test]
    fn an_unreadable_reset_time_is_an_error_not_a_window_without_a_pace() {
        // A changed timestamp format must not read as "just reset": that is the
        // same `--` a healthy fresh window shows, so it would go unnoticed
        // forever. Absent is a window without pace; unreadable is an error.
        let body = r#"{"limits":[
            {"kind":"session","group":"session","percent":4,
             "resets_at":"1786718400","scope":null}
        ]}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("1786718400"), "{err}");
    }

    #[test]
    fn an_unreadable_reset_time_in_the_scalar_fallback_is_also_an_error() {
        // The legacy shape gets the same treatment; it is the older of the two
        // and therefore the likelier one to drift.
        let body = r#"{"five_hour":{"utilization":9,"resets_at":"not a timestamp"}}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("not a timestamp"), "{err}");
    }

    #[test]
    fn a_model_scoped_cap_in_an_unknown_group_counts_down_but_reports_no_pace() {
        // A group this adapter cannot measure must not borrow some other
        // window's length — but the reset time it did send is real, and
        // `Window` counts down from an end with no start. Dropping it would
        // print strictly less than the response gave us.
        let body = r#"{"limits":[
            {"kind":"quarterly","group":"quarterly","percent":50,
             "resets_at":"2026-08-14T14:40:00Z",
             "scope":{"model":{"display_name":"Fable"}}}
        ]}"#;
        let scoped = parse(body).unwrap().scoped;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].window.used_percent, 50.0);
        assert_eq!(scoped[0].window.start_ms, None, "no span, so no pace");
        assert_eq!(scoped[0].window.end_ms, Some(1_786_718_400_000));
        assert_eq!(scoped[0].window.pace(1_786_700_000_000), None);
    }

    #[test]
    fn an_unreadable_reset_time_in_an_unknown_group_is_still_caught() {
        // The span is unknown, but the timestamp is right there — not reading
        // it would leave the one arm that never notices a format change.
        let body = r#"{"limits":[
            {"kind":"quarterly","group":"quarterly","percent":50,
             "resets_at":"1786718400","scope":{"model":{"display_name":"Fable"}}}
        ]}"#;
        let err = parse(body).unwrap_err().to_string();
        assert!(err.contains("1786718400"), "{err}");
    }

    #[test]
    fn one_unreadable_reset_time_does_not_delete_the_windows_that_parsed() {
        // Going dark on the whole account is a worse outage than the `--` this
        // guards against: the user loses the burn warning the tool exists for.
        // This is `main`'s "one bad key must not hide the other plans", applied
        // to the windows within one account.
        let body = r#"{"limits":[
            {"kind":"session","group":"session","percent":4,
             "resets_at":"2026-08-14T14:40:00Z","scope":null},
            {"kind":"weekly_all","group":"weekly","percent":78,
             "resets_at":"1786718400","scope":null}
        ]}"#;
        let usage = parse(body).unwrap();
        assert_eq!(usage.interval.as_ref().unwrap().used_percent, 4.0);
        assert!(usage.weekly.is_none(), "the unreadable one is dropped");
        // Surviving the bad window is right; hiding that it happened is not. A
        // `-` in the weekly column is what a provider with no weekly budget
        // prints, so without a reason the two are indistinguishable and a
        // response-shape change goes unnoticed while any window still parses.
        let reason = usage
            .degraded
            .expect("the dropped window must be accounted for");
        assert!(reason.contains("1786718400"), "{reason}");
    }

    #[test]
    fn a_second_session_entry_with_an_unreadable_reset_time_does_not_delete_the_first() {
        // `take` returning None must mean "nothing to add", never "clear what
        // is already there" — otherwise one appended malformed duplicate erases
        // a window that parsed cleanly, which inverts the rule it documents.
        let body = r#"{"limits":[
            {"kind":"session","group":"session","percent":4,
             "resets_at":"2026-08-14T14:40:00Z","scope":null},
            {"kind":"session","group":"session","percent":99,
             "resets_at":"1786718400","scope":null}
        ]}"#;
        let usage = parse(body).unwrap();
        assert_eq!(
            usage
                .interval
                .expect("the readable session window must survive")
                .used_percent,
            4.0
        );
    }

    #[test]
    fn a_note_can_report_drift_even_when_a_fallback_filled_the_gap() {
        // `unreadable` is set in the limits loop and never cleared, so the
        // scalar fallback can recover the very window that failed and still
        // leave a note. That is why the field reports "part of the response
        // could not be read" rather than promising a missing window — the
        // drift is real and worth saying even when the row is complete.
        let body = r#"{"limits":[
            {"kind":"session","group":"session","percent":4,
             "resets_at":"1786718400","scope":null}
        ],"five_hour":{"utilization":9,"resets_at":"2026-08-14T14:40:00Z"}}"#;
        let usage = parse(body).unwrap();
        assert_eq!(
            usage.interval.as_ref().unwrap().used_percent,
            9.0,
            "the fallback supplies the window"
        );
        assert!(
            usage.degraded.is_some(),
            "and the drift that forced it is still reported"
        );
    }

    #[test]
    fn a_clean_response_carries_no_degradation_note() {
        // The note must mean something when it appears, so it must be absent
        // on the ordinary path.
        assert!(parse(LIVE).unwrap().degraded.is_none());
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
