#!/usr/bin/env bash
# .claude/hooks/rebase-safety.sh — Enforce --rebase-merges and create backups before rebase
#
# Claude Code PreToolUse hook for the Bash tool.
# This hook intercepts git rebase commands and:
# 1. Blocks rebases without --rebase-merges when the branch has merge commits
# 2. Creates timestamped backup before any rebase
#
# Per .claude/rules/workflow-git-rebase-safety.md:
# - Always use --rebase-merges when branch contains merge commits
# - Create main-<timestamp> backups before destructive operations
#
# Exit codes:
#   0 - Allow the tool call to proceed
#   2 - Block the tool call (with error message on stderr)

set -euo pipefail

# Read the tool call JSON from stdin
INPUT="$(cat)"

# Extract the command from the JSON input using python3 for reliable parsing
# Falls back to simple grep if python3 is unavailable
COMMAND=""
if command -v python3 >/dev/null 2>&1; then
    COMMAND="$(printf '%s' "${INPUT}" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('tool_input',{}).get('command',''))" 2>/dev/null || true)"
elif command -v jq >/dev/null 2>&1; then
    COMMAND="$(printf '%s' "${INPUT}" | jq -r '.tool_input.command // empty' 2>/dev/null || true)"
fi

# If we couldn't parse JSON, check the raw input as a fallback
if [[ -z "${COMMAND}" ]]; then
    COMMAND="${INPUT}"
fi

# Only check git rebase commands
if ! printf '%s' "${COMMAND}" | grep -q 'git rebase'; then
    exit 0
fi

# If --rebase-merges is already present, allow (still create backup below)
if printf '%s' "${COMMAND}" | grep -q -- '--rebase-merges'; then
    # Still create a backup before the rebase
    TIMESTAMP="$(date +%Y-%m-%d-%H-%M-%S 2>/dev/null || echo "unknown")"
    CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")"

    if [[ "${CURRENT_BRANCH}" != "unknown" ]] && [[ "${CURRENT_BRANCH}" != "HEAD" ]]; then
        git branch "${CURRENT_BRANCH}-backup-${TIMESTAMP}" 2>/dev/null || true
    fi

    exit 0
fi

# git rebase without --rebase-merges
# Check if the current branch has merge commits
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")"

HAS_MERGE_COMMITS=false
MERGE_COUNT=0
if [[ "${CURRENT_BRANCH}" != "unknown" ]] && [[ "${CURRENT_BRANCH}" != "HEAD" ]]; then
    # Check for merge commits on this branch since diverging from default branch
    # Resolve default branch: try local main first, fall back to origin/main
    DEFAULT_BRANCH="main"
    if ! git rev-parse --verify main >/dev/null 2>&1; then
        DEFAULT_BRANCH="origin/main"
    fi
    MERGE_COUNT="$(git log "${DEFAULT_BRANCH}..HEAD" --merges --oneline 2>/dev/null | wc -l || echo 0)"
    if [[ "${MERGE_COUNT}" -gt 0 ]]; then
        HAS_MERGE_COMMITS=true
    fi
fi

if [[ "${HAS_MERGE_COMMITS}" == "true" ]]; then
    echo "BLOCKED: git rebase without --rebase-merges on branch with merge commits." >&2
    echo "Per .claude/rules/workflow-git-rebase-safety.md:" >&2
    echo "  - ALWAYS use --rebase-merges when branch contains merge commits" >&2
    echo "  - Plain 'git rebase' destroys merge history (BANNED)" >&2
    echo "" >&2
    echo "Your branch has ${MERGE_COUNT} merge commit(s)." >&2
    echo "Use: git rebase --rebase-merges origin/main" >&2
    exit 2
fi

# No merge commits, but still create a backup before rebase
TIMESTAMP="$(date +%Y-%m-%d-%H-%M-%S 2>/dev/null || echo "unknown")"
if [[ "${CURRENT_BRANCH}" != "unknown" ]] && [[ "${CURRENT_BRANCH}" != "HEAD" ]]; then
    git branch "${CURRENT_BRANCH}-backup-${TIMESTAMP}" 2>/dev/null || true
fi

exit 0
