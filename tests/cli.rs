//! End-to-end tests against the built binary.
//!
//! Every case here is hermetic: `XDG_CONFIG_HOME` points at a scratch directory
//! and every provider variable is removed from the child environment, so no test
//! can reach the network or accidentally depend on the developer's own keys.
//! Accounts whose credential is unset fail at resolution, which is before any
//! HTTP call is made — that is what keeps these fast and offline.
//!
//! `HOME` is pinned to the scratch directory for the same reason, and it is what
//! hides a real `~/.claude/.credentials.json` from the Anthropic fallback.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_llm-watcher");

/// Variables the fallback path probes. All are stripped so a developer running
/// `cargo test` with real keys exported gets the same result as CI.
const PROVIDER_VARS: [&str; 6] = [
    "MINIMAX_API_KEY",
    "MINIMAX_MAX_API_KEY",
    "ZHIPU_API_KEY",
    "ZAI_API_KEY",
    // Either of these alone conjures an Anthropic account, and a live token
    // would make `cargo test` query the real endpoint.
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CONFIG_DIR",
];

/// A scratch config directory that deletes itself when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "llm-watcher-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("llm-watcher")).expect("create scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write_config(&self, toml: &str) {
        fs::write(self.0.join("llm-watcher").join("config.toml"), toml).expect("write config");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(scratch: &Scratch, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env("XDG_CONFIG_HOME", scratch.path())
        // HOME is the fallback base; pinning it stops a missing XDG from
        // reaching the developer's real config.
        .env("HOME", scratch.path());
    for var in PROVIDER_VARS {
        command.env_remove(var);
    }
    command.output().expect("run llm-watcher")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is utf-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is utf-8")
}

/// A config whose accounts all reference variables that are never set, so the
/// run fails at key resolution without touching the network.
const KEYLESS_CONFIG: &str = r#"
[[account]]
name = "mini-one"
provider = "minimax"
key_env = "LLM_WATCHER_TEST_ABSENT_A"

[[account]]
name = "glm-one"
provider = "zai"
key_env = "LLM_WATCHER_TEST_ABSENT_B"
"#;

#[test]
fn version_and_help_succeed() {
    let scratch = Scratch::new();
    for flag in ["--version", "--help"] {
        let output = run(&scratch, &[flag]);
        assert!(output.status.success(), "{flag} should exit 0");
        assert!(!stdout(&output).is_empty(), "{flag} should print something");
    }
}

#[test]
fn help_explains_what_pace_means() {
    let scratch = Scratch::new();
    let text = stdout(&run(&scratch, &["--help"]));
    assert!(text.contains("1.0x"), "the ratio needs explaining: {text}");
}

#[test]
fn with_nothing_configured_the_error_says_how_to_configure_it() {
    let scratch = Scratch::new();
    let output = run(&scratch, &[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty(), "no table without accounts");
    let err = stderr(&output);
    assert!(err.contains("no accounts configured"), "{err}");
    assert!(err.contains("MINIMAX_API_KEY"), "{err}");
    assert!(
        err.contains("config.toml"),
        "name the file to create: {err}"
    );
}

#[test]
fn a_config_defining_no_accounts_is_rejected_by_name() {
    let scratch = Scratch::new();
    scratch.write_config("# nothing here\n");
    let output = run(&scratch, &[]);

    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("no [[account]] entries"), "{err}");
    assert!(err.contains("config.toml"), "{err}");
}

#[test]
fn a_malformed_provider_names_the_offending_account_and_the_file() {
    let scratch = Scratch::new();
    scratch.write_config("[[account]]\nname=\"typo\"\nprovider=\"minmax\"\nkey_env=\"K\"\n");
    let output = run(&scratch, &[]);

    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("typo"), "{err}");
    assert!(err.contains("config.toml"), "{err}");
}

#[test]
fn every_account_failing_exits_two_but_still_reports_each_row() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--json"]);

    assert_eq!(output.status.code(), Some(2), "nothing could be read");

    let reports: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let reports = reports.as_array().expect("a JSON array");
    assert_eq!(reports.len(), 2, "a dead key still gets a row");
    for report in reports {
        assert!(report["error"].is_string(), "{report}");
        assert!(report["interval"].is_null(), "no data alongside an error");
    }
    assert_eq!(reports[0]["name"], "mini-one");
    assert_eq!(reports[0]["provider"], "minimax");
    assert_eq!(reports[1]["provider"], "zai");
}

#[test]
fn the_error_names_the_variable_the_user_has_to_set() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let text = stdout(&run(&scratch, &["--json"]));
    assert!(text.contains("LLM_WATCHER_TEST_ABSENT_A"), "{text}");
}

#[test]
fn json_goes_to_stdout_and_progress_never_contaminates_it() {
    // The spinner writes to stderr and erases itself. If it ever leaked onto
    // stdout, `llm-watcher --json | jq` would break — so stdout must parse as
    // JSON with nothing before or after it.
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--json"]);
    let text = stdout(&output);

    assert!(
        text.starts_with('['),
        "stray output before the JSON: {text:?}"
    );
    assert!(
        text.trim_end().ends_with(']'),
        "stray output after: {text:?}"
    );
    assert!(
        !text.contains('\r'),
        "no cursor control on stdout: {text:?}"
    );
    assert!(!text.contains('\x1b'), "no escapes on stdout: {text:?}");
    serde_json::from_str::<serde_json::Value>(&text).expect("stdout is pure JSON");
}

#[test]
fn the_table_prints_a_header_and_one_row_per_account() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let text = stdout(&run(&scratch, &[]));

    assert!(text.contains("PLAN"), "{text}");
    assert!(text.contains("5H WINDOW"), "{text}");
    assert!(text.contains("WEEKLY"), "{text}");
    assert!(text.contains("mini-one"), "{text}");
    assert!(text.contains("glm-one"), "{text}");
    assert_eq!(text.lines().count(), 3, "header plus two rows: {text}");
}

#[test]
fn no_color_suppresses_every_escape_sequence() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--no-color"]);
    assert!(!stdout(&output).contains('\x1b'), "{:?}", stdout(&output));
    assert!(!stderr(&output).contains('\x1b'), "{:?}", stderr(&output));
}

#[test]
fn piped_output_carries_no_progress_animation_on_either_stream() {
    // Neither stream is a terminal under `Command::output`, so the spinner must
    // disable itself entirely rather than write frames into a redirected log.
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &[]);
    for stream in [stdout(&output), stderr(&output)] {
        assert!(!stream.contains('⠋'), "spinner frame leaked: {stream:?}");
        assert!(
            !stream.contains("querying"),
            "status line leaked: {stream:?}"
        );
    }
}

#[test]
fn an_account_filter_selects_only_the_named_plan() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--json", "--account", "glm-one"]);

    let reports: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let reports = reports.as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["name"], "glm-one");
}

#[test]
fn the_account_filter_is_repeatable() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(
        &scratch,
        &["--json", "--account", "glm-one", "--account", "mini-one"],
    );
    let reports: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 2);
}

#[test]
fn an_account_filter_matching_nothing_is_an_error_not_an_empty_table() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--account", "does-not-exist"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout(&output).is_empty(),
        "an empty table would look like success"
    );
    let err = stderr(&output);
    assert!(err.contains("does-not-exist"), "{err}");
}

#[test]
fn the_account_filter_is_exact_not_a_prefix_match() {
    // A prefix match would silently report the wrong plan.
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    assert_eq!(run(&scratch, &["--account", "glm"]).status.code(), Some(2));
    assert_eq!(
        run(&scratch, &["--account", "glm-on"]).status.code(),
        Some(2)
    );
}

#[test]
fn a_bad_threshold_argument_is_rejected_by_the_parser() {
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let output = run(&scratch, &["--threshold", "not-a-number"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("not-a-number"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_failure_message_names_the_variable_never_a_key_value() {
    // Keys stay in the environment precisely so they never reach argv or shell
    // history. The unset-key path is the one failure that can be provoked
    // offline, and it must identify the variable by name only.
    let scratch = Scratch::new();
    scratch.write_config(KEYLESS_CONFIG);
    let combined = {
        let output = run(&scratch, &["--json"]);
        stdout(&output) + &stderr(&output)
    };
    assert!(combined.contains("LLM_WATCHER_TEST_ABSENT_A"), "{combined}");
    assert!(
        combined.contains("not set in the environment"),
        "{combined}"
    );
}

#[test]
fn an_anthropic_account_needs_no_key_env_in_the_config() {
    // A Claude subscription has no API key. Requiring key_env would make the
    // provider impossible to configure at all.
    let scratch = Scratch::new();
    scratch.write_config("[[account]]\nname = \"claude-max\"\nprovider = \"anthropic\"\n");
    let output = run(&scratch, &[]);

    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        !combined.contains("key_env"),
        "a missing key_env must not be an error here: {combined}"
    );
    assert!(combined.contains("claude-max"), "{combined}");
}

#[test]
fn an_anthropic_account_with_no_credentials_fails_before_any_request() {
    // HOME is the scratch dir, so there is no credential store to find. The row
    // must report that rather than hang on a network call.
    let scratch = Scratch::new();
    scratch.write_config("[[account]]\nname = \"claude\"\nprovider = \"anthropic\"\n");
    let output = run(&scratch, &[]);

    assert_eq!(output.status.code(), Some(2), "every account failed");
    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        combined.contains("claude") && combined.contains("log in"),
        "the row should say how to fix it: {combined}"
    );
}

#[test]
fn claude_and_anthropic_name_the_same_provider() {
    for alias in ["anthropic", "claude", "claude-code"] {
        let scratch = Scratch::new();
        scratch.write_config(&format!(
            "[[account]]\nname = \"x\"\nprovider = \"{alias}\"\n"
        ));
        let combined = {
            let output = run(&scratch, &[]);
            format!("{}{}", stdout(&output), stderr(&output))
        };
        assert!(
            !combined.contains("unknown provider"),
            "{alias} should be accepted: {combined}"
        );
    }
}

#[test]
fn an_anthropic_row_does_not_disturb_the_existing_two_columns() {
    // The scoped sub-row is rendered per account. With an Anthropic account
    // alongside the others the header must still be one line and each account
    // still one row, or the table has silently gained a column.
    let scratch = Scratch::new();
    scratch.write_config(
        "[[account]]\nname = \"mini\"\nprovider = \"minimax\"\nkey_env = \"LLM_WATCHER_TEST_ABSENT_A\"\n\
         [[account]]\nname = \"claude\"\nprovider = \"anthropic\"\n",
    );
    let text = stdout(&run(&scratch, &[]));

    assert!(
        text.contains("5H WINDOW") && text.contains("WEEKLY"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 3, "header plus two rows: {text}");
}
