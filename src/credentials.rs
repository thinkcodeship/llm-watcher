//! Claude Code OAuth credential lookup.
//!
//! MiniMax and Z.ai authenticate with a long-lived API key, which is why the rest
//! of this tool names keys by environment variable and reads them at runtime.
//! Anthropic subscriptions do not work that way: a Max plan has no API key. The
//! credential is a short-lived OAuth access token that Claude Code obtains at
//! login and stores itself, so reaching the quota endpoint means reading the
//! store Claude Code already wrote.
//!
//! This module only ever *reads*. Refreshing an expired token would mean writing
//! the store back, racing whatever Claude Code process is running, and risking
//! the refresh token a logged-in session depends on. An expired token is
//! reported as an expired token.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Environment variable holding a raw token, as written by `claude setup-token`.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Set to any non-empty value to skip the Keychain lookup entirely.
///
/// The other two sources can be redirected by environment — [`TOKEN_ENV`] holds
/// a token directly, and the store path follows `CLAUDE_CONFIG_DIR` or `HOME`.
/// The Keychain follows neither, so without this a test harness on macOS
/// resolves the developer's real credentials and queries the live endpoint.
pub const NO_KEYCHAIN_ENV: &str = "LLM_WATCHER_NO_KEYCHAIN";

/// Keychain service name Claude Code stores its credential document under.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// How much of `security`'s own diagnosis to carry into the error, in
/// characters. It shares a table cell with the account name, so it is bounded.
const DETAIL_LIMIT: usize = 160;

/// Whether a [`NO_KEYCHAIN_ENV`] value switches the Keychain lookup off.
///
/// Split from the environment read for the reason [`default_path_from`] is:
/// mutating real process environment from tests races every other test in the
/// binary, and asserting on the ambient value additionally fails for anyone who
/// has this documented switch exported in their own shell.
fn keychain_disabled_by(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// Whether [`NO_KEYCHAIN_ENV`] switches the Keychain lookup off.
///
/// Deliberately not `cfg`-gated: the Keychain call it guards only exists on
/// macOS, but the decision itself is platform-independent and therefore
/// testable everywhere.
fn keychain_disabled() -> bool {
    keychain_disabled_by(std::env::var_os(NO_KEYCHAIN_ENV))
}

/// Which Claude Code credential store to read.
///
/// A token held directly in an environment variable does not go through here —
/// there is no store to parse, so [`crate::config`] uses it as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `CLAUDE_CODE_OAUTH_TOKEN`, then the default config dir, then the Keychain.
    Default,
    /// A specific credentials file — how a second Claude account is described.
    File(PathBuf),
}

/// A resolved token plus the plan it belongs to, when the store recorded one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub token: String,
    pub subscription_type: Option<String>,
}

/// Default credentials path: `$CLAUDE_CONFIG_DIR/.credentials.json`, else
/// `~/.claude/.credentials.json`. Split from the environment lookup so the
/// precedence is testable.
pub fn default_path_from(
    claude_config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let base = claude_config_dir.or_else(|| home.map(|h| h.join(".claude")))?;
    Some(base.join(".credentials.json"))
}

/// `$CLAUDE_CONFIG_DIR/.credentials.json`, else `~/.claude/.credentials.json`.
pub fn default_path() -> Option<PathBuf> {
    default_path_from(
        std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Expand a leading `~/`, so a config file can say `~/.claude/...` and mean it.
pub fn expand_tilde_from(path: &str, home: Option<PathBuf>) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match home {
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_from(path, std::env::var_os("HOME").map(PathBuf::from))
}

#[derive(Debug, Deserialize)]
struct Store {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Debug, Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    /// Expiry in epoch milliseconds. Absent on older stores, in which case the
    /// token is used and the endpoint decides.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    /// `max`, `pro`, … Carried so an error can name the plan.
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

/// Pull the access token out of a credential document, refusing an expired one.
pub fn parse_store(text: &str, now_ms: i64) -> Result<Credential> {
    let store: Store = serde_json::from_str(text)
        .context("the Claude Code credential store is not the expected JSON")?;

    let oauth = store.oauth.ok_or_else(|| {
        anyhow!("the credential store has no claudeAiOauth entry — run `claude` to log in")
    })?;

    let token = oauth
        .access_token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow!("the credential store has no access token — run `claude` to log in")
        })?;

    // An expired token comes back from the endpoint as an opaque 429, so it is
    // caught here where the reason can actually be named. Refreshing is Claude
    // Code's job, not this tool's.
    if let Some(expires_at) = oauth.expires_at {
        if expires_at <= now_ms {
            let ago = crate::pace::format_duration(now_ms - expires_at);
            return Err(anyhow!(
                "the Claude Code OAuth token expired {ago} ago — run any `claude` command to refresh it"
            ));
        }
    }

    Ok(Credential {
        token,
        subscription_type: oauth.subscription_type,
    })
}

impl Source {
    /// Resolve this source to a usable token.
    pub fn resolve(&self, now_ms: i64) -> Result<Credential> {
        match self {
            Self::File(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                parse_store(&text, now_ms).with_context(|| format!("reading {}", path.display()))
            }
            Self::Default => resolve_default(now_ms),
        }
    }
}

fn resolve_default(now_ms: i64) -> Result<Credential> {
    if let Ok(token) = from_env(TOKEN_ENV) {
        return Ok(Credential {
            token,
            subscription_type: None,
        });
    }

    // Track why the file attempt failed, so an expired token is not reported as
    // a missing one after the Keychain also comes up empty.
    let mut failure = None;
    if let Some(path) = default_path() {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_store(&text, now_ms) {
                Ok(credential) => return Ok(credential),
                Err(e) => failure = Some(e.context(format!("reading {}", path.display()))),
            },
            // A missing file is the normal case on a machine that logs in
            // through the Keychain, so it does not outrank the Keychain's error.
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                failure =
                    Some(anyhow::Error::from(e).context(format!("reading {}", path.display())))
            }
            Err(_) => {}
        }
    }

    // Skipped wholesale when switched off, so a harness that has redirected the
    // other two sources is not silently answered by the real login.
    if !keychain_disabled() {
        match keychain_store() {
            Some(Ok(text)) => {
                return parse_store(&text, now_ms).context("reading the macOS Keychain")
            }
            Some(Err(e)) if failure.is_none() => failure = Some(e),
            _ => {}
        }
    }

    Err(failure.unwrap_or_else(|| {
        anyhow!(
            "no Claude Code credentials found — run `claude` to log in, or set {TOKEN_ENV}{}",
            default_path()
                .map(|p| format!(" (looked in {})", p.display()))
                .unwrap_or_default()
        )
    }))
}

fn from_env(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(anyhow!("{name} is not set in the environment")),
    }
}

/// Interpret a finished `security find-generic-password` invocation.
///
/// Split from the call itself so the mapping is testable: the invocation is
/// macOS-only, but deciding what its exit status and stderr mean is not.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn interpret_keychain_output(
    success: bool,
    status: &str,
    stdout: Vec<u8>,
    stderr: &[u8],
) -> Result<String> {
    if success {
        // Not `from_utf8_lossy`: replacement characters would reach
        // `parse_store` as valid UTF-8 and be reported as malformed JSON, one
        // layer below where the problem actually is.
        return String::from_utf8(stdout)
            .map_err(|e| anyhow!("the macOS Keychain returned non-UTF-8 bytes: {e}"));
    }
    // A denied access prompt, a locked login keychain, a corrupt keychain and a
    // genuinely absent item all exit non-zero. Only the last is fixed by
    // logging in again, so `security`'s own diagnosis is carried through rather
    // than replaced by a guess at which of them happened.
    //
    // Collapsed and bounded because it lands in a single table cell: `main`
    // prints one line per row, and `security` writes free-form multi-line
    // diagnostics, so an embedded newline would silently grow the table by a
    // row — the invariant tests/cli.rs pins as one line per account.
    let stderr = String::from_utf8_lossy(stderr);
    let detail = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail = match detail.char_indices().nth(DETAIL_LIMIT) {
        Some((cut, _)) => format!("{}…", &detail[..cut]),
        None => detail,
    };
    Err(anyhow!(
        "could not read {KEYCHAIN_SERVICE:?} from the macOS Keychain ({status}{}{}) \
         — if the item is simply absent, run `claude` to log in",
        if detail.is_empty() { "" } else { ": " },
        detail
    ))
}

#[cfg(target_os = "macos")]
fn keychain_store() -> Option<Result<String>> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output();
    Some(match output {
        Ok(out) => interpret_keychain_output(
            out.status.success(),
            &out.status.to_string(),
            out.stdout,
            &out.stderr,
        ),
        Err(e) => Err(anyhow!("could not run `security`: {e}")),
    })
}

#[cfg(not(target_os = "macos"))]
fn keychain_store() -> Option<Result<String>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_786_718_400_000;

    #[test]
    fn the_keychain_is_switched_off_by_a_non_empty_value_only() {
        // The Keychain is the one source no scratch directory can redirect, so
        // the tests/cli.rs harness switches it off outright. A blank value must
        // not count, or an accidentally-exported empty variable would silently
        // disable the real lookup for a user.
        use std::ffi::OsString;
        assert!(
            !keychain_disabled_by(None),
            "unset must leave the Keychain enabled"
        );
        assert!(
            !keychain_disabled_by(Some(OsString::from(""))),
            "a blank value must leave the Keychain enabled"
        );
        assert!(
            keychain_disabled_by(Some(OsString::from("1"))),
            "a non-empty value must switch the Keychain off"
        );
        assert!(
            keychain_disabled_by(Some(OsString::from("0"))),
            "any non-empty value counts, including one that reads as false"
        );
    }

    #[test]
    fn the_no_keychain_variable_keeps_the_name_the_test_harness_spells_out() {
        // tests/cli.rs runs against the built binary and cannot import this
        // constant, so it hard-codes the string. Renaming the value here would
        // compile, pass every test, and silently un-isolate that harness on a
        // macOS machine that is logged in.
        assert_eq!(NO_KEYCHAIN_ENV, "LLM_WATCHER_NO_KEYCHAIN");
    }

    #[test]
    fn a_failed_keychain_lookup_carries_the_reason_security_gave() {
        // A denied access prompt, a locked login keychain and a genuinely
        // absent item all exit non-zero, and only the last is fixed by logging
        // in again. Reporting them all as "no entry" sends the other two around
        // a loop that cannot terminate, with the real diagnosis discarded.
        let err = interpret_keychain_output(
            false,
            "exit status: 36",
            Vec::new(),
            b"SecKeychainSearchCopyNext: User interaction is not allowed.\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("exit status: 36"), "{err}");
        assert!(err.contains("User interaction is not allowed"), "{err}");
    }

    #[test]
    fn a_multi_line_keychain_diagnostic_is_collapsed_onto_one_line() {
        // The error lands in a single table cell, and `main` prints one line
        // per row — an embedded newline silently grows the table by a row,
        // breaking the one-line-per-account invariant tests/cli.rs pins.
        let err = interpret_keychain_output(
            false,
            "exit status: 51",
            Vec::new(),
            b"SecKeychainSearchCopyNext: authorization denied.\nUser interaction is not allowed.\n",
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains('\n'), "{err:?}");
        assert!(err.contains("authorization denied"), "{err}");
        assert!(err.contains("User interaction is not allowed"), "{err}");
    }

    #[test]
    fn an_enormous_keychain_diagnostic_is_truncated() {
        // Unbounded subprocess output would push the account name off the row.
        let err = interpret_keychain_output(false, "exit status: 1", Vec::new(), &vec![b'x'; 4096])
            .unwrap_err()
            .to_string();
        assert!(err.contains('…'), "{err}");
        assert!(err.len() < 400, "still {} chars", err.len());
    }

    #[test]
    fn a_keychain_document_that_is_not_utf8_is_named_as_such() {
        // from_utf8_lossy would substitute U+FFFD and hand parse_store a string
        // that is valid UTF-8 but not valid JSON, reporting a byte-level
        // problem as a malformed credential store one layer too late.
        let err = interpret_keychain_output(true, "exit status: 0", vec![0xff, 0xfe], b"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-UTF-8"), "{err}");
    }

    #[test]
    fn a_successful_keychain_lookup_returns_the_document_unchanged() {
        let text =
            interpret_keychain_output(true, "exit status: 0", b"{\"a\":1}".to_vec(), b"").unwrap();
        assert_eq!(text, "{\"a\":1}");
    }

    fn store(expires_at: i64) -> String {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-test",
                "refreshToken":"sk-ant-ort01-test","expiresAt":{expires_at},
                "scopes":["user:inference"],"subscriptionType":"max",
                "rateLimitTier":"default_claude_max_20x"}}}}"#
        )
    }

    #[test]
    fn reads_the_token_and_plan_from_a_live_shaped_store() {
        let credential = parse_store(&store(NOW + 3_600_000), NOW).unwrap();
        assert_eq!(credential.token, "sk-ant-oat01-test");
        assert_eq!(credential.subscription_type.as_deref(), Some("max"));
    }

    #[test]
    fn an_expired_token_is_named_as_expired_not_sent_to_the_endpoint() {
        // The endpoint answers a stale token with an opaque 429, which reads as
        // a rate limit rather than as "log in again".
        let err = parse_store(&store(NOW - 7_200_000), NOW)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expired"), "{err}");
        assert!(err.contains("2h"), "{err}");
    }

    #[test]
    fn a_token_expiring_one_millisecond_from_now_is_still_accepted() {
        assert_eq!(
            parse_store(&store(NOW + 1), NOW).unwrap().token,
            "sk-ant-oat01-test"
        );
    }

    #[test]
    fn a_store_without_an_expiry_is_used_rather_than_rejected() {
        let text = r#"{"claudeAiOauth":{"accessToken":"tok"}}"#;
        assert_eq!(parse_store(text, NOW).unwrap().token, "tok");
    }

    #[test]
    fn a_store_with_no_oauth_entry_says_to_log_in() {
        let err = parse_store(r#"{"mcpOAuth":{}}"#, NOW)
            .unwrap_err()
            .to_string();
        assert!(err.contains("claudeAiOauth"), "{err}");
    }

    #[test]
    fn a_blank_token_is_treated_as_absent() {
        let text = r#"{"claudeAiOauth":{"accessToken":"   "}}"#;
        assert!(parse_store(text, NOW).is_err());
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_store("not json", NOW).is_err());
        assert!(parse_store("", NOW).is_err());
    }

    #[test]
    fn an_unrelated_mcp_oauth_block_does_not_confuse_the_lookup() {
        // The real file carries MCP server tokens alongside the Claude one, and
        // picking the wrong accessToken authenticates as the wrong service.
        let text = r#"{"mcpOAuth":{"posthog|abc":{"accessToken":"WRONG"}},
                       "claudeAiOauth":{"accessToken":"RIGHT","subscriptionType":"max"}}"#;
        assert_eq!(parse_store(text, NOW).unwrap().token, "RIGHT");
    }

    #[test]
    fn the_token_is_trimmed_because_the_keychain_appends_a_newline() {
        // `security find-generic-password -w` emits a trailing newline, which
        // would otherwise ride along into the Authorization header.
        let text = "{\"claudeAiOauth\":{\"accessToken\":\"tok\\n\"}}";
        assert_eq!(parse_store(text, NOW).unwrap().token, "tok");
    }

    #[test]
    fn claude_config_dir_wins_over_home() {
        let path = default_path_from(
            Some(PathBuf::from("/custom/claude")),
            Some(PathBuf::from("/home/example")),
        );
        assert_eq!(
            path,
            Some(PathBuf::from("/custom/claude/.credentials.json"))
        );
    }

    #[test]
    fn home_supplies_the_default_claude_directory() {
        let path = default_path_from(None, Some(PathBuf::from("/home/example")));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/example/.claude/.credentials.json"))
        );
    }

    #[test]
    fn with_neither_variable_there_is_no_default_path() {
        assert_eq!(default_path_from(None, None), None);
    }

    #[test]
    fn a_leading_tilde_expands_against_home() {
        assert_eq!(
            expand_tilde_from(
                "~/.claude/.credentials.json",
                Some(PathBuf::from("/home/example"))
            ),
            PathBuf::from("/home/example/.claude/.credentials.json")
        );
    }

    #[test]
    fn only_a_leading_tilde_is_special() {
        // An absolute path and a tilde mid-string both pass through untouched.
        let home = Some(PathBuf::from("/home/example"));
        assert_eq!(
            expand_tilde_from("/etc/creds.json", home.clone()),
            PathBuf::from("/etc/creds.json")
        );
        assert_eq!(
            expand_tilde_from("/tmp/a~b.json", home),
            PathBuf::from("/tmp/a~b.json")
        );
    }
}
