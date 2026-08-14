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
    config_path_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// `$XDG_CONFIG_HOME/llm-watcher/config.toml`, falling back to `$HOME/.config`.
/// Split from the environment lookup so the precedence is testable.
fn config_path_from(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = xdg_config_home.or_else(|| home.map(|h| h.join(".config")))?;
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
    fallback_accounts_from(|name| std::env::var(name).ok())
}

/// The fallback list filtered by whichever variables `lookup` reports as set.
///
/// Takes a lookup rather than reading the environment directly: mutating real
/// process environment from tests races every other test in the binary.
fn fallback_accounts_from(lookup: impl Fn(&str) -> Option<String>) -> Vec<Account> {
    let mut seen_zai = false;
    FALLBACK_ACCOUNTS
        .iter()
        .filter(|(_, _, env)| lookup(env).is_some_and(|v| !v.trim().is_empty()))
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

    /// A lookup backed by a fixed list, standing in for the process environment.
    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn no_environment_variables_means_no_accounts() {
        assert!(fallback_accounts_from(env_of(&[])).is_empty());
    }

    #[test]
    fn two_minimax_variables_produce_two_separately_keyed_accounts() {
        // The whole reason accounts are named: one variable cannot describe two
        // Max plans, and collapsing them would report one plan twice.
        let accounts = fallback_accounts_from(env_of(&[
            ("MINIMAX_API_KEY", "a"),
            ("MINIMAX_MAX_API_KEY", "b"),
        ]));
        assert_eq!(accounts.len(), 2);
        assert_ne!(accounts[0].name, accounts[1].name);
        assert_ne!(accounts[0].key_env, accounts[1].key_env);
    }

    #[test]
    fn the_two_zai_variable_names_never_double_count_one_plan() {
        // ZHIPU_API_KEY and ZAI_API_KEY are two names for the same account.
        let accounts =
            fallback_accounts_from(env_of(&[("ZHIPU_API_KEY", "a"), ("ZAI_API_KEY", "b")]));
        assert_eq!(accounts.len(), 1, "one plan must not appear as two rows");
        assert_eq!(accounts[0].key_env, "ZHIPU_API_KEY", "first match wins");
        assert_eq!(accounts[0].provider, Provider::Zai);
    }

    #[test]
    fn either_zai_variable_alone_is_enough() {
        for var in ["ZHIPU_API_KEY", "ZAI_API_KEY"] {
            let accounts = fallback_accounts_from(env_of(&[(var, "k")]));
            assert_eq!(accounts.len(), 1, "{var} should stand on its own");
            assert_eq!(accounts[0].key_env, var);
            assert_eq!(accounts[0].name, "glm");
        }
    }

    #[test]
    fn a_variable_set_to_whitespace_counts_as_unset() {
        // An empty key produces a confusing 401 rather than an honest "not set".
        for blank in ["", "   ", "\t"] {
            assert!(
                fallback_accounts_from(env_of(&[("MINIMAX_API_KEY", blank)])).is_empty(),
                "{blank:?} must not create an account"
            );
        }
    }

    #[test]
    fn both_providers_can_be_discovered_at_once() {
        let accounts =
            fallback_accounts_from(env_of(&[("MINIMAX_API_KEY", "a"), ("ZAI_API_KEY", "b")]));
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].provider, Provider::Minimax);
        assert_eq!(accounts[1].provider, Provider::Zai);
    }

    #[test]
    fn xdg_config_home_wins_over_home() {
        let path = config_path_from(Some("/xdg".into()), Some("/home/u".into())).unwrap();
        assert_eq!(path, PathBuf::from("/xdg/llm-watcher/config.toml"));
    }

    #[test]
    fn home_supplies_dot_config_when_xdg_is_unset() {
        let path = config_path_from(None, Some("/home/u".into())).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/u/.config/llm-watcher/config.toml")
        );
    }

    #[test]
    fn with_neither_variable_there_is_no_config_path() {
        assert!(config_path_from(None, None).is_none());
    }

    #[test]
    fn a_config_that_is_not_toml_is_an_error() {
        assert!(parse_config("{\"account\": []}").is_err());
        assert!(parse_config("[[account]").is_err());
    }

    #[test]
    fn account_order_from_the_file_is_preserved() {
        // The table prints rows in this order, so a reshuffle would be visible.
        let text = r#"
            [[account]]
            name = "z-first"
            provider = "zai"
            key_env = "K1"
            [[account]]
            name = "m-second"
            provider = "minimax"
            key_env = "K2"
        "#;
        let accounts = parse_config(text).unwrap();
        assert_eq!(accounts[0].name, "z-first");
        assert_eq!(accounts[1].name, "m-second");
    }

    #[test]
    fn a_key_variable_that_is_set_but_blank_is_reported_as_unset() {
        let account = Account {
            name: "x".into(),
            provider: Provider::Minimax,
            key_env: "LLM_WATCHER_TEST_BLANK_VAR".into(),
        };
        // Safety: this variable is used by no other test in the binary.
        std::env::set_var("LLM_WATCHER_TEST_BLANK_VAR", "   ");
        let result = account.key();
        std::env::remove_var("LLM_WATCHER_TEST_BLANK_VAR");
        assert!(result.is_err(), "a blank key must not reach the provider");
    }

    #[test]
    fn a_set_key_variable_is_returned_verbatim() {
        let account = Account {
            name: "x".into(),
            provider: Provider::Zai,
            key_env: "LLM_WATCHER_TEST_SET_VAR".into(),
        };
        std::env::set_var("LLM_WATCHER_TEST_SET_VAR", "sk-secret");
        let key = account.key();
        std::env::remove_var("LLM_WATCHER_TEST_SET_VAR");
        assert_eq!(key.unwrap(), "sk-secret");
    }
}
