//! Account configuration.
//!
//! Accounts are named explicitly because multi-account is the whole point: one
//! `MINIMAX_API_KEY` variable cannot describe two MiniMax Max plans, and a tool
//! that silently tracks one of them while reporting "MiniMax" is worse than no
//! tool at all.
//!
//! Keys are referenced by environment variable name and read at runtime. They are
//! never stored in the config file and never accepted as a command-line argument —
//! argv is visible in `ps` and lands in shell history.

use crate::provider::Provider;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Tried in order when no config file exists, so the tool works before anything
/// is written to disk. Missing variables are skipped, not reported as errors.
const FALLBACK_ACCOUNTS: &[(&str, Provider, &str)] = &[
    ("minimax", Provider::Minimax, "MINIMAX_API_KEY"),
    ("minimax-max", Provider::Minimax, "MINIMAX_MAX_API_KEY"),
    ("glm", Provider::Zai, "ZHIPU_API_KEY"),
    ("glm", Provider::Zai, "ZAI_API_KEY"),
];

#[derive(Debug, Clone)]
pub struct Account {
    pub name: String,
    pub provider: Provider,
    pub key_env: String,
}

impl Account {
    /// Resolve the key from the environment. A variable that exists but is empty
    /// is treated as absent — an empty key produces a confusing 401 otherwise.
    pub fn key(&self) -> Result<String> {
        match std::env::var(&self.key_env) {
            Ok(v) if !v.trim().is_empty() => Ok(v),
            _ => Err(anyhow!("{} is not set in the environment", self.key_env)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default, rename = "account")]
    accounts: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    name: String,
    provider: String,
    key_env: String,
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("llm-watcher").join("config.toml"))
}

/// Load configured accounts, falling back to well-known environment variables.
pub fn load() -> Result<Vec<Account>> {
    if let Some(path) = config_path() {
        if path.exists() {
            return load_file(&path);
        }
    }
    Ok(fallback_accounts())
}

fn load_file(path: &Path) -> Result<Vec<Account>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let accounts = parse_config(&text).with_context(|| format!("parsing {}", path.display()))?;
    if accounts.is_empty() {
        return Err(anyhow!("{} defines no [[account]] entries", path.display()));
    }
    Ok(accounts)
}

pub fn parse_config(text: &str) -> Result<Vec<Account>> {
    let file: File = toml::from_str(text)?;
    file.accounts
        .into_iter()
        .map(|e| {
            Ok(Account {
                provider: Provider::parse_name(&e.provider)
                    .with_context(|| format!("account {:?}", e.name))?,
                name: e.name,
                key_env: e.key_env,
            })
        })
        .collect()
}

fn fallback_accounts() -> Vec<Account> {
    let mut seen_zai = false;
    FALLBACK_ACCOUNTS
        .iter()
        .filter(|(_, _, env)| std::env::var(env).is_ok_and(|v| !v.trim().is_empty()))
        // ZHIPU_API_KEY and ZAI_API_KEY are two names for the same account; taking
        // both would double-count one plan as two.
        .filter(|(_, provider, _)| {
            if *provider == Provider::Zai {
                !std::mem::replace(&mut seen_zai, true)
            } else {
                true
            }
        })
        .map(|(name, provider, env)| Account {
            name: (*name).to_string(),
            provider: *provider,
            key_env: (*env).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_multi_account_config() {
        let text = r#"
            [[account]]
            name     = "minimax-work"
            provider = "minimax"
            key_env  = "MINIMAX_API_KEY"

            [[account]]
            name     = "minimax-max"
            provider = "minimax"
            key_env  = "MINIMAX_MAX_API_KEY"

            [[account]]
            name     = "glm"
            provider = "zai"
            key_env  = "ZHIPU_API_KEY"
        "#;
        let accounts = parse_config(text).unwrap();
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[1].name, "minimax-max");
        assert_eq!(accounts[1].key_env, "MINIMAX_MAX_API_KEY");
        assert_eq!(accounts[2].provider, Provider::Zai);
    }

    #[test]
    fn two_minimax_accounts_keep_distinct_key_variables() {
        // The onWatch failure mode: both plans collapsing onto one key.
        let text = r#"
            [[account]]
            name = "a"
            provider = "minimax"
            key_env = "MINIMAX_API_KEY"
            [[account]]
            name = "b"
            provider = "minimax"
            key_env = "MINIMAX_MAX_API_KEY"
        "#;
        let accounts = parse_config(text).unwrap();
        assert_ne!(accounts[0].key_env, accounts[1].key_env);
    }

    #[test]
    fn glm_and_zhipu_are_accepted_as_provider_aliases() {
        for alias in ["zai", "ZAI", "glm", "zhipu"] {
            let text = format!("[[account]]\nname=\"x\"\nprovider=\"{alias}\"\nkey_env=\"K\"\n");
            assert_eq!(parse_config(&text).unwrap()[0].provider, Provider::Zai);
        }
    }

    #[test]
    fn an_unknown_provider_names_the_offending_account() {
        let text = r#"
            [[account]]
            name = "typo-plan"
            provider = "minmax"
            key_env = "K"
        "#;
        let err = format!("{:#}", parse_config(text).unwrap_err());
        assert!(err.contains("typo-plan"), "{err}");
        assert!(err.contains("minmax"), "{err}");
    }

    #[test]
    fn a_config_with_no_accounts_parses_to_an_empty_list() {
        assert!(parse_config("").unwrap().is_empty());
    }

    #[test]
    fn a_missing_field_is_an_error_not_a_default() {
        let text = "[[account]]\nname=\"x\"\nprovider=\"minimax\"\n";
        assert!(parse_config(text).is_err());
    }

    #[test]
    fn an_unset_key_variable_is_reported_by_name() {
        let account = Account {
            name: "x".into(),
            provider: Provider::Minimax,
            key_env: "LLM_WATCHER_DEFINITELY_UNSET_VAR".into(),
        };
        let err = account.key().unwrap_err().to_string();
        assert!(err.contains("LLM_WATCHER_DEFINITELY_UNSET_VAR"), "{err}");
    }
}
