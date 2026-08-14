//! Account configuration.
//!
//! Accounts are named explicitly because multi-account is the whole point: one
//! `MINIMAX_API_KEY` variable cannot describe two MiniMax Max plans, and a tool
//! that silently tracks one of them while reporting "MiniMax" is worse than no
//! tool at all.
//!
//! API keys are referenced by environment variable name and read at runtime.
//! They are never stored in the config file and never accepted as a command-line
//! argument — argv is visible in `ps` and lands in shell history.
//!
//! Anthropic is the exception, because a Claude subscription has no API key: the
//! credential is an OAuth token that Claude Code obtains at login and stores
//! itself. Those accounts therefore name a *credential store* instead of a
//! variable, and [`crate::credentials`] reads it. That store is only ever read,
//! never written, and the key still never touches argv.

use crate::credentials::{self, Source};
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

/// Name given to the Claude Code account discovered without a config file.
const CLAUDE_FALLBACK_NAME: &str = "claude";

/// Where one account's credential comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// A named environment variable holding an API key.
    Env(String),
    /// Claude Code's OAuth store — the default location, or an explicit path
    /// when one machine holds more than one Claude account.
    ClaudeCode(Option<PathBuf>),
}

#[derive(Debug, Clone)]
pub struct Account {
    pub name: String,
    pub provider: Provider,
    pub credential: CredentialRef,
}

impl Account {
    /// Resolve this account's credential.
    ///
    /// A variable that exists but is empty is treated as absent — an empty key
    /// produces a confusing 401 otherwise. `now_ms` is needed because an OAuth
    /// token carries an expiry that is worth naming before the request is sent.
    pub fn key(&self, now_ms: i64) -> Result<String> {
        match &self.credential {
            // Trimmed for the reason `credentials::from_env` trims: a token
            // pasted in, or produced by `$(cat …)`, carries a trailing newline,
            // and a newline cannot go into an Authorization header. Both paths
            // to CLAUDE_CODE_OAUTH_TOKEN must agree about this.
            CredentialRef::Env(name) => match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
                _ => Err(anyhow!("{name} is not set in the environment")),
            },
            CredentialRef::ClaudeCode(path) => {
                let source = match path {
                    Some(path) => Source::File(path.clone()),
                    None => Source::Default,
                };
                Ok(source.resolve(now_ms)?.token)
            }
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
    /// Required for providers that authenticate with an API key; meaningless for
    /// Anthropic, which has none.
    #[serde(default)]
    key_env: Option<String>,
    /// An explicit Claude Code credentials file. Only meaningful for Anthropic.
    #[serde(default)]
    credentials_file: Option<String>,
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
            let provider = Provider::parse_name(&e.provider)
                .with_context(|| format!("account {:?}", e.name))?;
            let credential =
                credential_ref(&e, provider).with_context(|| format!("account {:?}", e.name))?;
            Ok(Account {
                provider,
                name: e.name,
                credential,
            })
        })
        .collect()
}

/// Decide where an entry's credential comes from, rejecting combinations that
/// cannot work.
///
/// `key_env` stays mandatory for the API-key providers: dropping it silently
/// would turn a typo'd config into an account that never authenticates.
fn credential_ref(entry: &Entry, provider: Provider) -> Result<CredentialRef> {
    match provider {
        Provider::Anthropic => {
            if entry.key_env.is_some() && entry.credentials_file.is_some() {
                return Err(anyhow!("set either key_env or credentials_file, not both"));
            }
            // A key_env on an Anthropic account means "this variable holds an
            // OAuth token", which is how a CI runner supplies one.
            match (&entry.key_env, &entry.credentials_file) {
                (Some(name), _) => Ok(CredentialRef::Env(name.clone())),
                (None, Some(path)) => Ok(CredentialRef::ClaudeCode(Some(
                    credentials::expand_tilde(path),
                ))),
                (None, None) => Ok(CredentialRef::ClaudeCode(None)),
            }
        }
        Provider::Minimax | Provider::Zai => {
            if entry.credentials_file.is_some() {
                return Err(anyhow!(
                    "credentials_file only applies to anthropic accounts; {} authenticates with an API key in key_env",
                    provider.as_str()
                ));
            }
            entry
                .key_env
                .clone()
                .map(CredentialRef::Env)
                .ok_or_else(|| anyhow!("missing field `key_env`"))
        }
    }
}

fn fallback_accounts() -> Vec<Account> {
    fallback_accounts_from(
        |name| std::env::var(name).ok(),
        credentials::default_path().is_some_and(|p| p.exists()),
    )
}

/// The fallback list filtered by whichever variables `lookup` reports as set,
/// plus a Claude Code account when one is logged in.
///
/// Takes a lookup rather than reading the environment directly: mutating real
/// process environment from tests races every other test in the binary.
fn fallback_accounts_from(
    lookup: impl Fn(&str) -> Option<String>,
    claude_store_exists: bool,
) -> Vec<Account> {
    let mut seen_zai = false;
    let mut accounts: Vec<Account> = FALLBACK_ACCOUNTS
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
            credential: CredentialRef::Env((*env).to_string()),
        })
        .collect();

    // Anthropic has no API key to look for, so its presence is decided by an
    // explicit token variable or by Claude Code having logged in on this machine.
    let token_env = lookup(credentials::TOKEN_ENV).is_some_and(|v| !v.trim().is_empty());
    if token_env || claude_store_exists {
        accounts.push(Account {
            name: CLAUDE_FALLBACK_NAME.to_string(),
            provider: Provider::Anthropic,
            credential: if token_env {
                CredentialRef::Env(credentials::TOKEN_ENV.to_string())
            } else {
                CredentialRef::ClaudeCode(None)
            },
        });
    }

    accounts
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
        assert_eq!(
            accounts[1].credential,
            CredentialRef::Env("MINIMAX_MAX_API_KEY".into())
        );
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
        assert_ne!(accounts[0].credential, accounts[1].credential);
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
    fn a_missing_key_env_is_an_error_for_an_api_key_provider() {
        // key_env became optional so Anthropic could omit it. It must stay
        // mandatory everywhere else, or a typo'd config silently produces an
        // account that can never authenticate.
        for provider in ["minimax", "zai"] {
            let text = format!("[[account]]\nname=\"x\"\nprovider=\"{provider}\"\n");
            let err = format!("{:#}", parse_config(&text).unwrap_err());
            assert!(err.contains("key_env"), "{provider}: {err}");
            assert!(err.contains("\"x\""), "{provider}: {err}");
        }
    }

    #[test]
    fn an_anthropic_account_needs_no_key_env_at_all() {
        // A Claude subscription has no API key; requiring one would make the
        // provider unconfigurable.
        let text = "[[account]]\nname=\"claude-max\"\nprovider=\"anthropic\"\n";
        let accounts = parse_config(text).unwrap();
        assert_eq!(accounts[0].provider, Provider::Anthropic);
        assert_eq!(accounts[0].credential, CredentialRef::ClaudeCode(None));
    }

    #[test]
    fn an_explicit_credentials_file_selects_a_second_claude_account() {
        let text = "[[account]]\nname=\"work\"\nprovider=\"anthropic\"\n\
                    credentials_file=\"/opt/work/.credentials.json\"\n";
        assert_eq!(
            parse_config(text).unwrap()[0].credential,
            CredentialRef::ClaudeCode(Some(PathBuf::from("/opt/work/.credentials.json")))
        );
    }

    #[test]
    fn an_anthropic_key_env_holds_an_oauth_token_for_ci() {
        let text = "[[account]]\nname=\"ci\"\nprovider=\"anthropic\"\n\
                    key_env=\"CLAUDE_CODE_OAUTH_TOKEN\"\n";
        assert_eq!(
            parse_config(text).unwrap()[0].credential,
            CredentialRef::Env("CLAUDE_CODE_OAUTH_TOKEN".into())
        );
    }

    #[test]
    fn naming_both_credential_sources_is_rejected_rather_than_ranked() {
        // Silently preferring one would make the ignored line look effective.
        let text = "[[account]]\nname=\"x\"\nprovider=\"anthropic\"\n\
                    key_env=\"T\"\ncredentials_file=\"/tmp/c.json\"\n";
        let err = format!("{:#}", parse_config(text).unwrap_err());
        assert!(err.contains("not both"), "{err}");
    }

    #[test]
    fn a_credentials_file_on_an_api_key_provider_is_rejected() {
        // It would be read as configured and then never consulted.
        let text = "[[account]]\nname=\"x\"\nprovider=\"minimax\"\n\
                    key_env=\"K\"\ncredentials_file=\"/tmp/c.json\"\n";
        let err = format!("{:#}", parse_config(text).unwrap_err());
        assert!(err.contains("anthropic"), "{err}");
    }

    #[test]
    fn an_unset_key_variable_is_reported_by_name() {
        let account = Account {
            name: "x".into(),
            provider: Provider::Minimax,
            credential: CredentialRef::Env("LLM_WATCHER_DEFINITELY_UNSET_VAR".into()),
        };
        let err = account.key(0).unwrap_err().to_string();
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
        assert!(fallback_accounts_from(env_of(&[]), false).is_empty());
    }

    #[test]
    fn two_minimax_variables_produce_two_separately_keyed_accounts() {
        // The whole reason accounts are named: one variable cannot describe two
        // Max plans, and collapsing them would report one plan twice.
        let accounts = fallback_accounts_from(
            env_of(&[("MINIMAX_API_KEY", "a"), ("MINIMAX_MAX_API_KEY", "b")]),
            false,
        );
        assert_eq!(accounts.len(), 2);
        assert_ne!(accounts[0].name, accounts[1].name);
        assert_ne!(accounts[0].credential, accounts[1].credential);
    }

    #[test]
    fn the_two_zai_variable_names_never_double_count_one_plan() {
        // ZHIPU_API_KEY and ZAI_API_KEY are two names for the same account.
        let accounts = fallback_accounts_from(
            env_of(&[("ZHIPU_API_KEY", "a"), ("ZAI_API_KEY", "b")]),
            false,
        );
        assert_eq!(accounts.len(), 1, "one plan must not appear as two rows");
        assert_eq!(
            accounts[0].credential,
            CredentialRef::Env("ZHIPU_API_KEY".into()),
            "first match wins"
        );
        assert_eq!(accounts[0].provider, Provider::Zai);
    }

    #[test]
    fn either_zai_variable_alone_is_enough() {
        for var in ["ZHIPU_API_KEY", "ZAI_API_KEY"] {
            let accounts = fallback_accounts_from(env_of(&[(var, "k")]), false);
            assert_eq!(accounts.len(), 1, "{var} should stand on its own");
            assert_eq!(accounts[0].credential, CredentialRef::Env(var.into()));
            assert_eq!(accounts[0].name, "glm");
        }
    }

    #[test]
    fn a_logged_in_claude_code_supplies_an_anthropic_account_with_no_variables() {
        // A Claude subscription has no API key, so the store's existence is the
        // only signal that the plan is there to report.
        let accounts = fallback_accounts_from(env_of(&[]), true);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].provider, Provider::Anthropic);
        assert_eq!(accounts[0].name, "claude");
        assert_eq!(accounts[0].credential, CredentialRef::ClaudeCode(None));
    }

    #[test]
    fn an_oauth_token_variable_outranks_the_credential_store() {
        // Both present: the explicit variable is the deliberate override.
        let accounts = fallback_accounts_from(
            env_of(&[("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-x")]),
            true,
        );
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            accounts[0].credential,
            CredentialRef::Env("CLAUDE_CODE_OAUTH_TOKEN".into())
        );
    }

    #[test]
    fn without_a_store_or_a_token_no_anthropic_account_is_invented() {
        // Reporting a Claude row on a machine that never logged in would be a
        // permanent error row for a plan the user may not even have.
        let accounts = fallback_accounts_from(env_of(&[("MINIMAX_API_KEY", "a")]), false);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].provider, Provider::Minimax);
    }

    #[test]
    fn anthropic_joins_the_other_providers_rather_than_replacing_them() {
        let accounts = fallback_accounts_from(
            env_of(&[("MINIMAX_API_KEY", "a"), ("ZHIPU_API_KEY", "b")]),
            true,
        );
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[2].provider, Provider::Anthropic);
    }

    #[test]
    fn a_variable_set_to_whitespace_counts_as_unset() {
        // An empty key produces a confusing 401 rather than an honest "not set".
        for blank in ["", "   ", "\t"] {
            assert!(
                fallback_accounts_from(env_of(&[("MINIMAX_API_KEY", blank)]), false).is_empty(),
                "{blank:?} must not create an account"
            );
        }
    }

    #[test]
    fn both_providers_can_be_discovered_at_once() {
        let accounts = fallback_accounts_from(
            env_of(&[("MINIMAX_API_KEY", "a"), ("ZAI_API_KEY", "b")]),
            false,
        );
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
            credential: CredentialRef::Env("LLM_WATCHER_TEST_BLANK_VAR".into()),
        };
        // Safety: this variable is used by no other test in the binary.
        std::env::set_var("LLM_WATCHER_TEST_BLANK_VAR", "   ");
        let result = account.key(0);
        std::env::remove_var("LLM_WATCHER_TEST_BLANK_VAR");
        assert!(result.is_err(), "a blank key must not reach the provider");
    }

    #[test]
    fn a_set_key_variable_is_returned_verbatim() {
        let account = Account {
            name: "x".into(),
            provider: Provider::Zai,
            credential: CredentialRef::Env("LLM_WATCHER_TEST_SET_VAR".into()),
        };
        std::env::set_var("LLM_WATCHER_TEST_SET_VAR", "sk-secret");
        let key = account.key(0);
        std::env::remove_var("LLM_WATCHER_TEST_SET_VAR");
        assert_eq!(key.unwrap(), "sk-secret");
    }

    #[test]
    fn a_key_variable_with_surrounding_whitespace_is_trimmed() {
        // `export CLAUDE_CODE_OAUTH_TOKEN=$(cat token.txt)` is the ordinary way
        // to supply one, and the trailing newline it brings cannot go into an
        // Authorization header. `credentials::from_env` already trims for this
        // reason; the two paths to the same variable must not disagree.
        let account = Account {
            name: "ci".into(),
            provider: Provider::Anthropic,
            credential: CredentialRef::Env("LLM_WATCHER_TEST_NEWLINE_VAR".into()),
        };
        // Safety: this variable is used by no other test in the binary.
        std::env::set_var("LLM_WATCHER_TEST_NEWLINE_VAR", "sk-ant-oat01-x\n");
        let key = account.key(0);
        std::env::remove_var("LLM_WATCHER_TEST_NEWLINE_VAR");
        assert_eq!(key.unwrap(), "sk-ant-oat01-x");
    }
}
