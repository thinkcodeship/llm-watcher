//! Provider adapters.
//!
//! Each provider exposes a quota endpoint with its own shape, its own auth
//! convention, and its own idea of which direction a percentage points. Adapters
//! normalize all of that into [`Usage`] before anything else sees it.

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
    /// The short rolling window (5 hours on both providers today).
    pub interval: Option<Window>,
    /// The weekly window.
    pub weekly: Option<Window>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Minimax,
    Zai,
}

impl Provider {
    pub fn parse_name(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "minimax" => Ok(Self::Minimax),
            "zai" | "glm" | "zhipu" => Ok(Self::Zai),
            other => Err(anyhow!(
                "unknown provider {other:?} (expected \"minimax\" or \"zai\")"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minimax => "minimax",
            Self::Zai => "zai",
        }
    }

    fn endpoint(&self) -> &'static str {
        match self {
            Self::Minimax => "https://api.minimax.io/v1/token_plan/remains",
            Self::Zai => "https://api.z.ai/api/monitor/usage/quota/limit",
        }
    }

    /// Auth header value. MiniMax wants `Bearer <key>`; Z.ai takes the raw token.
    /// Z.ai tolerates a `Bearer` prefix too, but the raw form is what its own
    /// clients send, so that is what we send.
    fn auth_header(&self, key: &str) -> String {
        match self {
            Self::Minimax => format!("Bearer {key}"),
            Self::Zai => key.to_string(),
        }
    }

    /// Parse a raw response body. Kept separate from [`Self::fetch`] so the
    /// parsers are testable against fixtures with no network.
    pub fn parse(&self, body: &str) -> Result<Usage> {
        match self {
            Self::Minimax => minimax::parse(body),
            Self::Zai => zai::parse(body),
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
