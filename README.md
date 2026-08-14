# llm-watcher

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust 2021](https://img.shields.io/badge/rust-2021%20edition-orange.svg)

Answers one question: **am I burning this coding plan too fast?**

```
$ llm-watcher
PLAN         5H WINDOW                        WEEKLY
minimax        4% used   1.2x  resets 4h 50m   49% used   0.8x  resets 2d 13h
minimax-max    0% used   0.0x  resets 4h 50m   38% used   0.6x  resets 2d 13h
glm            0% used     --                  20% used   0.6x  resets 4d 13h
```

Runs, prints, exits. No daemon, no database, no listening port, no background
process, and nothing written into `~/.claude/`.

## Reading a row

Every plan gets two windows — the rolling 5-hour cap and the weekly cap — and
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

### Countdowns

Reset times show the two largest units, zeros dropped — `2d 13h`, `4h 50m`,
`45m`, `30s`, and plain `7d` for a week untouched. Minutes remaining in a
two-day window is not a number anyone acts on, so it is not shown.

## Install

```bash
cargo install --path .
```

## Configure

With no config file it falls back to `MINIMAX_API_KEY`, `MINIMAX_MAX_API_KEY`, and
`ZHIPU_API_KEY` / `ZAI_API_KEY`, so it works before you write anything.

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
```

Keys are referenced by environment variable name and read at runtime. They are
never stored in the config and never accepted as a command-line argument — argv is
visible in `ps` and lands in shell history.

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
  }
]
```

</details>

## Providers

| Provider | Endpoint | Auth |
|----------|----------|------|
| MiniMax Token Plan | `GET https://api.minimax.io/v1/token_plan/remains` | `Authorization: Bearer <Subscription Key>` |
| Z.ai / GLM Coding Plan | `GET https://api.z.ai/api/monitor/usage/quota/limit` | `Authorization: <token>` — **no `Bearer` prefix** |

The asymmetry in that last column is real, not a typo. Getting it backwards
produces a `401` that reads exactly like a bad key.

### Anthropic Claude Max is not supported

No public endpoint exists. It is an open feature request,
[anthropics/claude-code#44328](https://github.com/anthropics/claude-code/issues/44328),
asking for exactly this: usage across multiple Max accounts, outside an active
session. Today the session cap, weekly cap and extra-usage balance live only in
the Claude Code status bar while a session runs.

The available workaround is to install a `statusLine` hook into
`~/.claude/settings.json` and scrape the payload Claude Code pipes through it.
This tool deliberately does not do that. Revisit if Anthropic ships an endpoint.

## What the responses actually look like

Neither provider documents its response body, and the third-party write-ups that
do are inconsistent with each other. What follows was captured live on 2026-08-14
from a MiniMax Max plan and a GLM Pro plan. Fixtures in `tests/fixtures/` are the
real documents.

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

## Verify

```bash
cargo test                          # 121 tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The suite is hermetic: the end-to-end tests in `tests/cli.rs` drive the built
binary with `XDG_CONFIG_HOME` pointed at a scratch directory and the provider
variables stripped from the child environment, so they neither reach the network
nor depend on whoever runs them having keys exported.

Then check the output against the vendor consoles —
[MiniMax](https://platform.minimax.io/console/usage) and
[Z.ai](https://z.ai) — because the console is the reference, not this table.

Two things worth confirming on a live run:

1. The two MiniMax rows show **different** numbers. Identical rows mean both
   accounts resolved to the same key, which is how a single `MINIMAX_API_KEY`
   variable silently tracks one plan while claiming to cover both.
2. Run twice ~10 minutes apart with no activity in between. Usage should not move.

## License

Released under the [MIT License](LICENSE) (SPDX identifier: `MIT`).

Copyright (c) 2026 Kanshiro LLC.

You may use, copy, modify, merge, publish, distribute, sublicense, and sell
copies of this software, provided the copyright notice and the permission
notice in [`LICENSE`](LICENSE) are included with any substantial portion of it.
The software comes with no warranty.
