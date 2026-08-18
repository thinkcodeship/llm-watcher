//! Statuspage.io status-page integration.
//!
//! Most LLM vendors host their status pages on Atlassian Statuspage and expose a
//! public, auth-free JSON endpoint at `/api/v2/summary.json`. This module
//! fetches and parses that response so a row can answer "is the provider up?"
//! alongside "am I overpacing?".
//!
//! The fetch and parse functions are separate, mirroring the provider adapters,
//! so the parser is testable against fixtures with no network.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Statuspage.io URL for Anthropic's public status page. Public, no auth.
pub const ANTHROPIC_URL: &str = "https://status.claude.com/api/v2/summary.json";

/// One snapshot of a vendor's status page, flattened from the wire format.
///
/// `status` is one of Statuspage.io's indicator strings:
/// `"none" | "minor" | "major" | "critical" | "maintenance" | "custom"`. New
/// buckets land occasionally; an unrecognised value passes through verbatim so
/// the row reflects what the server actually said.
#[derive(Debug, Clone, Serialize)]
pub struct StatusPageReport {
    pub status: String,
    /// Human-readable short description, e.g. `"Partial System Outage"`.
    pub description: String,
    /// Open (unresolved) incidents. Absent (not `null`) when the server sent
    /// an empty array — JSON consumers see the same shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incidents: Vec<Incident>,
    /// Active or upcoming scheduled maintenances.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scheduled_maintenances: Vec<Incident>,
}

/// One open incident or scheduled maintenance as Statuspage.io summarises it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub name: String,
    /// `"none" | "minor" | "major" | "critical"`.
    pub impact: String,
    /// `"investigating" | "identified" | "monitoring" | "resolved" | "postmortem"`.
    pub status: String,
    pub shortlink: String,
}

/// Wire shape returned by `/api/v2/summary.json`. The `status` field is a
/// nested object on the wire; the public `StatusPageReport` flattens it so
/// the row schema stays simple and the renderer can branch on a single string.
#[derive(Deserialize)]
struct WireStatusPage {
    status: WireIndicator,
    #[serde(default)]
    status_description: String,
    #[serde(default)]
    incidents: Vec<Incident>,
    #[serde(default)]
    scheduled_maintenances: Vec<Incident>,
}

#[derive(Deserialize)]
struct WireIndicator {
    indicator: String,
    #[serde(default)]
    description: String,
}

/// Fetch and parse a status-page summary. Network and parse failures return
/// `Err`; callers collapse to `None` so the rest of the row is unaffected.
pub fn fetch(url: &str) -> Result<StatusPageReport> {
    let body = crate::provider::http_get(url, None).with_context(|| format!("querying {url}"))?;
    parse(&body)
}

/// Strip ANSI escape sequences and C0 control characters from a value
/// crossing the JSON trust boundary. `serde_json` decodes `` JSON-escapes
/// to raw `\x1b` bytes before serde sees them, so a Statuspage.io response
/// that (intentionally or not) contains a CSI or OSC sequence in an
/// indicator, description, or incident name would otherwise reach the
/// operator's TTY verbatim via `format!` + `colored`'s wrappers.
///
/// CSI (`ESC [` ... final-byte in `0x40..=0x7e`) and OSC (`ESC ]` ... BEL)
/// sequences are dropped in their entirety; other C0 controls are dropped
/// except TAB and LF. Legitimate Statuspage content has none of either.
fn sanitize_for_tty(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if (0x40..=0x7e).contains(&(c as u32)) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    for c in chars.by_ref() {
                        if c == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Parse a captured `/api/v2/summary.json` body. Pure — no I/O — so the
/// fixtures in `tests/fixtures/` exercise every branch without a network.
pub fn parse(body: &str) -> Result<StatusPageReport> {
    let wire: WireStatusPage =
        serde_json::from_str(body).context("parsing status page summary.json")?;
    Ok(StatusPageReport {
        // The nested `status.description` is the canonical human text;
        // `status_description` is a duplicated top-level convenience field
        // that the server emits but the nested one shadows.
        status: sanitize_for_tty(&wire.status.indicator),
        description: sanitize_for_tty(&pick_description(
            &wire.status.description,
            &wire.status_description,
        )),
        incidents: wire
            .incidents
            .into_iter()
            .map(|i| Incident {
                name: sanitize_for_tty(&i.name),
                impact: i.impact,
                status: i.status,
                shortlink: i.shortlink,
            })
            .collect(),
        scheduled_maintenances: wire.scheduled_maintenances,
    })
}

fn pick_description(nested: &str, top_level: &str) -> String {
    if !nested.is_empty() {
        nested.to_string()
    } else if !top_level.is_empty() {
        top_level.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTIAL_OUTAGE: &str =
        include_str!("../tests/fixtures/status_claude_partial_outage.json");
    const OPERATIONAL: &str = include_str!("../tests/fixtures/status_claude_operational.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/status_claude_malformed.json");

    #[test]
    fn partial_outage_carries_the_open_incident() {
        let report = parse(PARTIAL_OUTAGE).expect("parses");
        assert_eq!(report.status, "major");
        assert_eq!(report.description, "Partial System Outage");
        assert_eq!(report.incidents.len(), 1);
        assert!(report.incidents[0].name.contains("API errors"));
        assert_eq!(report.incidents[0].impact, "major");
        assert_eq!(report.incidents[0].status, "identified");
        assert!(report.incidents[0].shortlink.starts_with("https://"));
        assert!(report.scheduled_maintenances.is_empty());
    }

    #[test]
    fn operational_summary_has_no_open_incidents() {
        let report = parse(OPERATIONAL).expect("parses");
        assert_eq!(report.status, "none");
        assert_eq!(report.description, "All Systems Operational");
        assert!(report.incidents.is_empty());
        assert!(report.scheduled_maintenances.is_empty());
    }

    #[test]
    fn a_malformed_body_returns_an_error_not_a_panic() {
        assert!(parse(MALFORMED).is_err());
    }

    #[test]
    fn empty_arrays_omit_themselves_from_json() {
        let report = parse(OPERATIONAL).expect("parses");
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("incidents"), "absent not null: {json}");
        assert!(!json.contains("scheduled_maintenances"), "{json}");
        assert!(!json.contains("null"), "{json}");
    }

    #[test]
    fn a_status_field_serialises_to_the_expected_shape() {
        // Statuspage.io can introduce new indicator strings over time; the
        // schema does not lock to an enum, so any value the server sends must
        // pass through. The output JSON is what consumers see, so its keys and
        // scalars are pinned here.
        let report = parse(PARTIAL_OUTAGE).unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(r#""status":"major""#), "{json}");
        assert!(
            json.contains(r#""description":"Partial System Outage""#),
            "{json}"
        );
        assert!(json.contains(r#""impact":"major""#), "{json}");
        assert!(
            json.contains(r#""shortlink":"https://status.claude.com/incidents/"#),
            "{json}"
        );
    }

    #[test]
    fn pick_description_prefers_the_nested_value() {
        assert_eq!(pick_description("nested", "top"), "nested");
        assert_eq!(pick_description("", "top"), "top");
        assert_eq!(pick_description("nested", ""), "nested");
        assert_eq!(pick_description("", ""), "");
    }

    #[test]
    fn escape_sequences_are_stripped_at_parse_time() {
        // `serde_json` decodes `` (the 6-char ASCII sequence
        // backslash-u-0-0-1-b) into a single raw `\x1b` byte inside the
        // resulting Rust String. We embed those 6 ASCII chars directly in
        // the JSON source so the source file carries no raw control bytes;
        // serde_json does the conversion.
        let body = r#"{
            "status": {"indicator": "majorc", "description": "d"},
            "status_description": "",
            "incidents": [
                {"name": "evillink",
                 "impact": "major", "status": "identified",
                 "shortlink": "https://status.claude.com/incidents/x"}
            ],
            "scheduled_maintenances": []
        }"#;
        let report = parse(body).unwrap();
        assert!(
            !report.status.contains('\u{1b}'),
            "indicator: {:?}",
            report.status
        );
        assert!(
            !report.incidents[0].name.contains('\u{1b}'),
            "name: {:?}",
            report.incidents[0].name
        );
        // Legitimate text survives the strip.
        assert!(report.incidents[0].name.contains("evillink"));
        assert!(report.status.starts_with("major"));
    }

    #[test]
    fn sanitize_for_tty_drops_csi_and_osc_sequences() {
        // CSI `ESC [ 2 J` clears the screen; OSC `ESC ] 8 ; ; URL BEL` is the
        // hyperlink sequence. Both should vanish; legitimate text around them
        // survives.
        let dirty = "beforec2Jmiddlelinkafter";
        let clean = sanitize_for_tty(dirty);
        assert!(!clean.contains('\u{1b}'), "got: {clean:?}");
        assert!(clean.contains("before"));
        assert!(clean.contains("middle"));
        assert!(clean.contains("after"));
        assert!(
            clean.contains("link"),
            "OSC hyperlink target stripped too: {clean:?}"
        );
    }
}
