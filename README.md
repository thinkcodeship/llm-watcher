# llm-watcher

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust 2021](https://img.shields.io/badge/rust-2021%20edition-orange.svg)

Project page: **[llm-watcher.thinkcodeship.com](https://llm-watcher.thinkcodeship.com/)**

Answers one question: **am I burning this coding plan too fast?**

```
$ llm-watcher
PLAN         5H WINDOW                        WEEKLY
minimax        4% used   0.1x  resets 1h 57m   49% used   0.8x  resets 2d 10h
minimax-max    4% used   0.1x  resets 1h 57m   39% used   0.6x  resets 2d 10h
glm            0% used     --                  20% used   0.5x  resets 4d 10h
claude        10% used   0.1x  resets 1h 37m   80% used   1.3x  resets 2d 14h
  └ Fable                                      61% used   1.0x  resets 2d 14h
```

Runs, prints, exits. No daemon, no database, no listening port, no background
process, and nothing written into `~/.claude/` — the Claude credential store is
read, never modified.

## Reading a row

Every plan gets at least two windows — the rolling 5-hour cap and the weekly cap — and
each window prints the same three things:

```
49% used   0.8x  resets 2d 13h
│          │     └─ how long until this window rolls over
│          └─ pace — quota spent against clock elapsed
└─ share of this window's quota already gone
```

### Pace

Providers report quota in incompatible units, so raw numbers do not compare.
Pace normalizes them:

```
pace = (fraction of quota consumed) / (fraction of window elapsed)
```

| Pace | Meaning |
|------|---------|
| `1.0x` | Spending exactly on budget — quota lands as the window resets |
| `> 1.0x` | Exhausts before reset |
| `< 1.0x` | Headroom remains |

Colour follows the same reading: green below `1.0x`, yellow from `1.0x`, red
from `1.5x`.

`--` means there is no honest signal yet: a window that just reset, or one whose
boundaries the provider did not report. A fabricated ratio there reads as an
emergency, so none is printed.

### Per-model caps

An indented `└` row is a weekly cap that applies to one model rather than the
whole plan:

```
claude        10% used   0.1x  resets 1h 37m   80% used   1.3x  resets 2d 14h
  └ Fable                                      61% used   1.0x  resets 2d 14h
```

Anthropic Max plans carry one per restricted model, and it can run out before the
all-model weekly does — so it is the row that tells you which limit will actually
stop you. `--threshold` counts these too. Providers with a single shared weekly
budget never print one.

### Countdowns

Reset times show the two largest units, zeros dropped — `2d 13h`, `4h 50m`,
`45m`, `30s`, and plain `7d` for a week untouched. Minutes remaining in a
two-day window is not a number anyone acts on, so it is not shown.

### Provider status

When a row's provider has a public status page, `llm-watcher` fetches it and
appends a marker when the indicator is anything other than `none`:

```
claude         0% used     --  resets 4h 55m    3% used  0.1x  resets 4d 18h  ⚠ minor — Degraded performance for Claude Opus 5 and Claude Haiku 4.5
  └ Fable                                      0% used  0.0x  resets 4d 18h
```

The marker carries the Statuspage.io indicator (`none` | `minor` | `major` |
`critical` | `maintenance` | `custom`), a colour band by severity, and the first
incident name when one is open. `none` — operational — produces no marker, so
a healthy run reads exactly as it did before this feature landed.

Only Anthropic is wired up today (→ `https://status.claude.com/api/v2/summary.json`).
Adding another provider is one match arm on `Provider::status_page_url()` plus a
fixture in `tests/fixtures/`. MiniMax and Z.ai return `None` until their pages
are verified.

The status fetch only runs when the quota fetch succeeds, so the
hermetic test suite (which fails accounts at key resolution) never reaches the
network. Failures collapse to an absent field, not an error — the quota row is
unaffected.

## Install

```bash
cargo install --path .
```

## Configure

With no config file it falls back to `MINIMAX_API_KEY`, `MINIMAX_MAX_API_KEY`, and
`ZHIPU_API_KEY` / `ZAI_API_KEY`, so it works before you write anything. If Claude
Code is logged in on this machine, a `claude` row appears too — no variable needed,
because a Claude subscription has no API key to set.

For more than one account per provider — the case this tool exists for — write
`~/.config/llm-watcher/config.toml`:

```toml
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
provider = "zai"        # aliases: glm, zhipu
key_env  = "ZHIPU_API_KEY"

[[account]]
name     = "claude"
provider = "anthropic"  # aliases: claude, claude-code
                        # no key_env — the token comes from Claude Code
```

API keys are referenced by environment variable name and read at runtime. They are
never stored in the config and never accepted as a command-line argument — argv is
visible in `ps` and lands in shell history.

### Anthropic credentials

There is no API key for a Claude subscription, so the token is read from wherever
Claude Code already put it, in this order:

1. `CLAUDE_CODE_OAUTH_TOKEN` — as written by `claude setup-token`.
2. `$CLAUDE_CONFIG_DIR/.credentials.json`, else `~/.claude/.credentials.json`.
3. The macOS Keychain (`Claude Code-credentials`).

Setting `LLM_WATCHER_NO_KEYCHAIN` to any non-empty value skips step 3. The first
two steps follow environment variables, so a sandbox can point them at a scratch
directory; the Keychain follows neither, and this is the switch that keeps it out
of a run that is meant to be isolated. The test suite sets it for that reason.

The store is **only ever read**. The token expires within hours, and refreshing it
would mean writing that file back — racing a running Claude Code session and
risking the refresh token it depends on. An expired token is reported as expired,
with the fix, rather than silently retried:

```
claude  (the Claude Code OAuth token expired 3h ago — run any `claude` command to refresh it)
```

For a second Claude account, point an entry at its own store:

```toml
[[account]]
name             = "claude-work"
provider         = "anthropic"
credentials_file = "~/.claude-work/.credentials.json"
```

On CI, where no store exists, put the token in a variable instead:

```toml
[[account]]
name     = "claude-ci"
provider = "anthropic"
key_env  = "CLAUDE_CODE_OAUTH_TOKEN"
```

Setting both `key_env` and `credentials_file` is rejected rather than ranked — a
silently-ignored line looks like it took effect.

## Usage

```bash
llm-watcher                            # table
llm-watcher --json                     # machine-readable
llm-watcher --account minimax-max      # one plan (repeatable)
llm-watcher --threshold 1.5            # exit 1 if any window is at or past 1.5x
llm-watcher --no-color
```

Accounts are queried one at a time, so a status line names the one in flight
rather than leaving the terminal silent:

```
⠹ [2/3] querying minimax-max
```

It draws on stderr and erases itself on the way out, and switches off entirely
when stderr is not a terminal. `--json` on stdout is therefore byte-identical
whether piped or not, and a redirected log collects no cursor escapes.

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Ran fine |
| `1` | `--threshold` was given and some window reached it |
| `2` | Every account failed, or the configuration is unusable |

A single failing account does not abort the run — the error lands on its own row
and the other plans still report.

`--threshold` plus `--json` is enough for a cron job or a waybar module without
any of this needing a daemon.

<details>
<summary><code>--json</code> output shape</summary>

`interval` is the 5-hour window and `weekly` the weekly one. Both are omitted
when the provider reports nothing for them, as are `pace` (no honest signal yet)
and `resets_in_ms` (no boundary reported). A failed account carries `error`
instead of either window.

`scoped` is the list of per-model weekly caps. It is omitted entirely when the
provider has none, so existing consumers are unaffected.

`degraded` appears only when part of the response could not be read while the
rest still parsed. The row keeps its numbers, and the string says what failed —
an omitted window otherwise looks exactly like one the provider never sends. It
reports the drift rather than promising a hole: a fallback may have recovered
the window whose primary source failed, leaving the row complete. Absent on
every healthy row and for every provider other than Anthropic, so existing
consumers are unaffected. In table mode the same note goes to stderr, after the
table, keeping stdout pipeable and the table one line per account.

```json
[
  {
    "name": "minimax",
    "provider": "minimax",
    "interval": { "used_percent": 4.0,  "pace": 0.0792, "resets_in_ms": 8913996 },
    "weekly":   { "used_percent": 49.0, "pace": 0.7585, "resets_in_ms": 214113996 }
  },
  {
    "name": "glm",
    "provider": "zai",
    "interval": { "used_percent": 0.0 },
    "weekly":   { "used_percent": 20.0, "pace": 0.5542, "resets_in_ms": 386526994 }
  },
  {
    "name": "claude",
    "provider": "anthropic",
    "interval": { "used_percent": 0.0,  "resets_in_ms": 17700000000 },
    "weekly":   { "used_percent": 3.0, "pace": 0.018, "resets_in_ms": 388800000 },
    "scoped": [
      { "label": "Fable", "used_percent": 0.0, "pace": 0.0, "resets_in_ms": 388800000 }
    ],
    "status_page": {
      "status": "minor",
      "description": "Degraded performance for Claude Opus 5 and Claude Haiku 4.5",
      "incidents": [
        { "name": "Degraded performance for Claude Opus 5 and Claude Haiku 4.5",
          "impact": "minor",
          "status": "identified",
          "shortlink": "https://status.claude.com/incidents/" }
      ]
    }
  }
]
```

</details>

## Providers

| Provider | Endpoint | Auth | Status page |
|----------|----------|------|--------------|
| MiniMax Token Plan | `GET https://api.minimax.io/v1/token_plan/remains` | `Authorization: Bearer <Subscription Key>` | — |
| Z.ai / GLM Coding Plan | `GET https://api.z.ai/api/monitor/usage/quota/limit` | `Authorization: <token>` — **no `Bearer` prefix** | — |
| Anthropic Claude (Pro, Max 5x, Max 20x) | `GET https://api.anthropic.com/api/oauth/usage` | `Authorization: Bearer <OAuth token>` | `status.claude.com` |

The asymmetry in that last column is real, not a typo. Getting it backwards
produces a `401` that reads exactly like a bad key.

Anthropic's is the *subscription* surface, not the pay-as-you-go API — no
`x-api-key`, and no `anthropic-beta` header is required. It is the same data
Claude Code's `/usage` command shows, so treat that as the reference.

## What the responses actually look like

No provider documents its response body, and the third-party write-ups that do
are inconsistent with each other. What follows was captured live on 2026-08-14
from a MiniMax Max plan, a GLM Pro plan, and a Claude Max 20x plan. Fixtures in
`tests/fixtures/` are the real documents.

<details>
<summary><b>MiniMax</b> — percentages are inverted, and <code>remains_time</code> is a trap</summary>

```json
{
  "model_remains": [
    { "model_name": "general",
      "start_time": 1786701600000, "end_time": 1786719600000,
      "remains_time": 17825021,
      "current_interval_total_count": 0, "current_interval_usage_count": 0,
      "current_interval_remaining_percent": 99,
      "weekly_start_time": 1786320000000, "weekly_end_time": 1786924800000,
      "current_weekly_remaining_percent": 51 },
    { "model_name": "video", "...": "independent quota" }
  ],
  "base_resp": { "status_code": 0, "status_msg": "success" }
}
```

- `current_*_remaining_percent` is **remaining**, not consumed. Read it the
  intuitive way and the burn signal is exactly backwards. Inverted at the parser.
- `model_remains` is a **top-level array**, not nested under `data`, and it carries
  one entry per model family. `general` is the coding quota; `video` sits on its
  own budget and reads as idle.
- `current_interval_usage_count` and `current_interval_total_count` are both `0` on
  a live Max plan. The counts are unusable; the percentages are the only real data.
- `remains_time` is a **countdown in milliseconds to the end of the window** — wall
  clock, not quota. It drains whether or not you send a single request.
  [MiniMax-M2.7#47](https://github.com/MiniMax-AI/MiniMax-M2.7/issues/47) reports
  this as a bug ("balance drains passively"); it is not a balance. This tool
  ignores the field and uses `end_time`, which says the same thing without
  inviting the misreading.

`status_code: 1004` almost always means the key is a pay-as-you-go API Key where a
Token Plan **Subscription Key** is required — take it from Account / Token Plan in
the console. The error message says so.

**Do not confuse this with `/coding_plan/remains`.**
`www.minimaxi.com/v1/api/openplatform/coding_plan/remains` is a browser endpoint
that rejects API keys with `"cookie is missing, log in again"`
([MiniMax-M2#88](https://github.com/MiniMax-AI/MiniMax-M2/issues/88), open since
2026-03-18). Same for billing history at `/account/amount`.

</details>

<details>
<summary><b>Z.ai</b> — percentages run the other way, and windows are keyed by unit code</summary>

```json
{ "code": 200, "success": true,
  "data": { "level": "pro", "limits": [
    { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 0 },
    { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 20,
      "nextResetTime": 1787097212998 },
    { "type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "...": "MCP tools" }
  ] } }
```

- `percentage` here is **consumed** — the opposite direction from MiniMax.
- `unit` is a period code (`3` hour, `6` week) and `number` the multiplier, so
  `unit: 3, number: 5` is the five-hour window. Windows are slotted by computed
  length rather than array position, so a reordered response cannot swap them.
- Only `nextResetTime` is given; the window start is one span back from it. An idle
  5-hour window carries no `nextResetTime` at all — usage still reports, pace does
  not.
- `TIME_LIMIT` is the monthly MCP-tool allowance (web search, zread), a separate
  budget. It is skipped.

**Unverified assumption:** that Z.ai's `percentage` means consumed. The evidence is
that an idle account reads `0` on the 5-hour window — as *remaining* that would
mean exhausted, yet requests still succeed — and Z.ai's docs describe the console
figure as a percentage of usage. To falsify: burn GLM quota and check whether the
number climbs (consumed) or falls (remaining). If it falls, invert it in
`src/provider/zai.rs`.

</details>

<details>
<summary><b>Anthropic</b> — only a reset time, and unreleased feature flags to ignore</summary>

```json
{
  "five_hour":  { "utilization": 4,  "resets_at": "2026-08-14T14:40:00.255510+00:00" },
  "seven_day":  { "utilization": 78, "resets_at": "2026-08-17T04:00:00.255540+00:00" },
  "seven_day_opus": null, "tangelo": null, "nimbus_quill": { "utilization": 0 },
  "limits": [
    { "kind": "session",       "group": "session", "percent": 4,
      "resets_at": "2026-08-14T14:40:00.255510+00:00", "scope": null },
    { "kind": "weekly_all",    "group": "weekly",  "percent": 78, "severity": "warning",
      "resets_at": "2026-08-17T04:00:00.255540+00:00", "scope": null, "is_active": true },
    { "kind": "weekly_scoped", "group": "weekly",  "percent": 61,
      "resets_at": "2026-08-17T04:00:00.255822+00:00",
      "scope": { "model": { "display_name": "Fable" } } }
  ],
  "extra_usage": { "is_enabled": false }, "spend": { "percent": 0 }
}
```

- `percent` / `utilization` is **consumed**, the same direction as Z.ai and the
  opposite of MiniMax. Nothing is inverted here.
- Timestamps are **RFC 3339 strings**, not epoch milliseconds like the other two.
  Converted in `src/rfc3339.rs`, which rejects a timestamp with no UTC offset —
  assuming local time would make the pace depend on the reader's `$TZ`.
- Only `resets_at` is given, never the start. The start is one span back: 5 hours
  for `session`, 7 days for both weekly windows.
- `limits[]` is preferred over the `five_hour` / `seven_day` scalars because it
  also carries the per-model cap. Entries are slotted on `group` plus whether
  `scope` is set — keying on `kind` would drop a window the moment Anthropic adds
  one, and keying on `group` alone would let the scoped cap overwrite the
  all-model weekly.
- **A bad or absent token answers `429`, not `401`.** Status code alone cannot
  distinguish "rate limited" from "log in again", which is why an expired token is
  caught from `expiresAt` before the request is ever sent.
- Fields named after unreleased features — `tangelo`, `nimbus_quill`,
  `cinder_cove`, `iguana_necktie`, `amber_ladder`, `omelette*` — are deliberately
  not parsed. They churn, and guessing at one invents a window that does not exist.

`seven_day_opus` and `seven_day_sonnet` exist as scalars but read `null` on this
plan; the per-model cap arrives through `weekly_scoped` instead, which is why the
scoped windows are a list rather than a single field.

</details>

## Verify

```bash
cargo test                          # 222 tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The suite is hermetic: the end-to-end tests in `tests/cli.rs` drive the built
binary with `XDG_CONFIG_HOME` **and `HOME`** pointed at a scratch directory and
every provider variable stripped from the child environment, so they neither reach
the network nor depend on whoever runs them having keys exported. Pinning `HOME`
is what hides a real `~/.claude/.credentials.json` from the Anthropic fallback —
without it, `cargo test` on a logged-in machine would query the live endpoint.

Then check the output against the vendor surfaces —
[MiniMax](https://platform.minimax.io/console/usage), [Z.ai](https://z.ai), and
Claude Code's own `/usage` — because those are the reference, not this table.

Three things worth confirming on a live run:

1. The two MiniMax rows show **different** numbers. Identical rows mean both
   accounts resolved to the same key, which is how a single `MINIMAX_API_KEY`
   variable silently tracks one plan while claiming to cover both.
2. Run twice ~10 minutes apart with no activity in between. Usage should not move.
3. The `claude` row matches `/usage` in Claude Code, and its `└` sub-row matches
   the per-model figure shown there.

## License

Released under the [MIT License](LICENSE) (SPDX identifier: `MIT`).

Copyright (c) 2026 Kanshiro LLC.

You may use, copy, modify, merge, publish, distribute, sublicense, and sell
copies of this software, provided the copyright notice and the permission
notice in [`LICENSE`](LICENSE) are included with any substantial portion of it.
The software comes with no warranty.
