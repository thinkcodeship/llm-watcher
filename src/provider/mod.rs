//! Provider adapters.
//!
//! Each provider exposes a quota endpoint with its own shape, its own auth
//! convention, and its own idea of which direction a percentage points. Adapters
//! normalize all of that into [`Usage`] before anything else sees it.

pub mod anthropic;
pub mod minimax;
pub mod zai;

use crate::pace::Window;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Normalized quota state for one account.
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    /// The short rolling window (5 hours on every provider today).
    pub interval: Option<Window>,
    /// The weekly window covering all models.
    pub weekly: Option<Window>,
    /// Per-model weekly caps, when a provider exposes them. Anthropic Max plans
    /// carry one per restricted model, and it can bind before [`Self::weekly`]
    /// does. Empty for providers with a single shared weekly budget.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<ScopedWindow>,
    /// Why a window is missing, when one failed to parse but others survived.
    ///
    /// A dropped window renders as the same `-` an absent one does, so without
    /// this a format change looks exactly like a provider that never sent the
    /// window. Distinct from `Report::error`, which means the whole account
    /// failed. Omitted from JSON when there is nothing to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
}

impl Usage {
    /// A usage report with no per-model caps — the shape every provider except
    /// Anthropic produces.
    pub fn new(interval: Option<Window>, weekly: Option<Window>) -> Self {
        Self {
            interval,
            weekly,
            scoped: Vec::new(),
            degraded: None,
        }
    }
}

/// A quota window that applies to one model rather than the whole plan.
#[derive(Debug, Clone, Serialize)]
pub struct ScopedWindow {
    /// The model the cap applies to, as the provider names it.
    pub label: String,
    #[serde(flatten)]
    pub window: Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Minimax,
    Zai,
    Anthropic,
}

impl Provider {
    pub fn parse_name(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "minimax" => Ok(Self::Minimax),
            "zai" | "glm" | "zhipu" => Ok(Self::Zai),
            "anthropic" | "claude" | "claude-code" => Ok(Self::Anthropic),
            other => Err(anyhow!(
                "unknown provider {other:?} (expected \"minimax\", \"zai\" or \"anthropic\")"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minimax => "minimax",
            Self::Zai => "zai",
            Self::Anthropic => "anthropic",
        }
    }

    fn endpoint(&self) -> &'static str {
        match self {
            Self::Minimax => "https://api.minimax.io/v1/token_plan/remains",
            Self::Zai => "https://api.z.ai/api/monitor/usage/quota/limit",
            Self::Anthropic => "https://api.anthropic.com/api/oauth/usage",
        }
    }

    /// Auth header value. MiniMax and Anthropic want `Bearer <token>`; Z.ai takes
    /// the raw token. Z.ai tolerates a `Bearer` prefix too, but the raw form is
    /// what its own clients send, so that is what we send.
    fn auth_header(&self, key: &str) -> String {
        match self {
            Self::Minimax | Self::Anthropic => format!("Bearer {key}"),
            Self::Zai => key.to_string(),
        }
    }

    /// Parse a raw response body. Kept separate from [`Self::fetch`] so the
    /// parsers are testable against fixtures with no network.
    pub fn parse(&self, body: &str) -> Result<Usage> {
        match self {
            Self::Minimax => minimax::parse(body),
            Self::Zai => zai::parse(body),
            Self::Anthropic => anthropic::parse(body),
        }
    }

    pub fn fetch(&self, key: &str) -> Result<Usage> {
        if key.trim().is_empty() {
            return Err(anyhow!("key is empty"));
        }
        let body = http_get(self.endpoint(), &self.auth_header(key))
            .with_context(|| format!("querying {}", self.endpoint()))?;
        self.parse(&body)
    }
}

fn http_get(url: &str, authorization: &str) -> Result<String> {
    // Non-2xx must not short-circuit: both providers return a machine-readable
    // error document that says more than the status code does.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();

    let mut response = agent
        .get(url)
        .header("Authorization", authorization)
        .header("Content-Type", "application/json")
        .header("Accept-Language", "en-US,en")
        .call()?;

    let status = response.status();
    let body = response.body_mut().read_to_string()?;

    if !status.is_success() && body.trim().is_empty() {
        return Err(anyhow!("HTTP {status} with an empty body"));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAX_LIVE: &str = include_str!("../../tests/fixtures/minimax_ok.json");
    const ZAI_LIVE: &str = include_str!("../../tests/fixtures/zai_ok.json");

    #[test]
    fn provider_names_are_case_insensitive() {
        for name in ["minimax", "MiniMax", "MINIMAX"] {
            assert_eq!(Provider::parse_name(name).unwrap(), Provider::Minimax);
        }
        for name in ["zai", "ZAI", "Glm", "ZHIPU"] {
            assert_eq!(Provider::parse_name(name).unwrap(), Provider::Zai);
        }
    }

    #[test]
    fn an_unknown_provider_names_both_the_input_and_the_valid_choices() {
        let err = Provider::parse_name("openai").unwrap_err().to_string();
        assert!(err.contains("openai"), "{err}");
        assert!(err.contains("minimax") && err.contains("zai"), "{err}");
    }

    #[test]
    fn whitespace_is_not_silently_accepted_as_a_provider() {
        // Trimming here would hide a malformed config rather than report it.
        assert!(Provider::parse_name(" minimax").is_err());
        assert!(Provider::parse_name("").is_err());
    }

    #[test]
    fn the_canonical_name_round_trips_back_to_the_same_provider() {
        for provider in [Provider::Minimax, Provider::Zai] {
            assert_eq!(Provider::parse_name(provider.as_str()).unwrap(), provider);
        }
        // Aliases collapse onto the canonical spelling, which is what the JSON
        // output and the table both show.
        assert_eq!(Provider::parse_name("glm").unwrap().as_str(), "zai");
    }

    #[test]
    fn minimax_takes_a_bearer_prefix_and_zai_takes_the_raw_token() {
        // Getting this backwards produces a 401 that reads like a bad key, so it
        // is worth pinning: the asymmetry is deliberate, not an oversight.
        assert_eq!(Provider::Minimax.auth_header("k"), "Bearer k");
        assert_eq!(Provider::Zai.auth_header("k"), "k");
        assert!(!Provider::Zai.auth_header("k").contains("Bearer"));
    }

    #[test]
    fn the_auth_header_passes_the_key_through_untouched() {
        let key = "sk-Abc.123_-xyz";
        assert!(Provider::Minimax.auth_header(key).ends_with(key));
        assert_eq!(Provider::Zai.auth_header(key), key);
    }

    #[test]
    fn endpoints_are_distinct_and_https() {
        let (minimax, zai) = (Provider::Minimax.endpoint(), Provider::Zai.endpoint());
        assert_ne!(minimax, zai);
        for url in [minimax, zai] {
            assert!(url.starts_with("https://"), "{url} must not be plaintext");
        }
    }

    #[test]
    fn each_provider_dispatches_to_its_own_parser() {
        assert!(Provider::Minimax.parse(MINIMAX_LIVE).is_ok());
        assert!(Provider::Zai.parse(ZAI_LIVE).is_ok());
    }

    #[test]
    fn a_provider_cannot_parse_the_other_ones_response() {
        // Both bodies are valid JSON, so a mixed-up pairing has to be caught by
        // the shape, not by the deserializer failing outright.
        assert!(Provider::Minimax.parse(ZAI_LIVE).is_err());
        assert!(Provider::Zai.parse(MINIMAX_LIVE).is_err());
    }

    #[test]
    fn an_empty_key_fails_before_any_request_is_made() {
        // No network is reachable in tests; this returning promptly is the proof
        // that the guard short-circuits ahead of the HTTP call.
        for key in ["", "   ", "\t\n"] {
            let err = Provider::Minimax.fetch(key).unwrap_err().to_string();
            assert!(err.contains("key is empty"), "{err}");
        }
        assert!(Provider::Zai.fetch("").is_err());
    }

    #[test]
    fn usage_serializes_both_windows_by_name() {
        let usage = Provider::Minimax.parse(MINIMAX_LIVE).unwrap();
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"interval\""), "{json}");
        assert!(json.contains("\"weekly\""), "{json}");
    }

    #[test]
    fn a_provider_serializes_as_its_lowercase_name() {
        assert_eq!(
            serde_json::to_string(&Provider::Zai).unwrap(),
            "\"zai\"",
            "the JSON output is a public interface"
        );
        assert_eq!(
            serde_json::to_string(&Provider::Minimax).unwrap(),
            "\"minimax\""
        );
    }
}
