#!/usr/bin/env bash
# .claude/hooks/prevent-main-commits.sh — Block direct commits and pushes to main
#
# Claude Code PreToolUse hook for the Bash tool. Intercepts `git commit` and
# `git push` commands and blocks them when they target a protected branch
# (main or master), so changes can only reach the default branch through a
# reviewed pull request.
#
# Per .claude/rules/workflow-never-commit-to-main.md:
# - NEVER commit directly to main (or master)
# - NEVER push directly to main (or master)
# - All changes to the default branch must go through a pull request
#
# Branch resolution (issue #1343): the branch is resolved for the repository
# the command actually TARGETS — honoring `git -C <path>`, `--git-dir <path>`,
# a `GIT_DIR=<path>` environment prefix, and a leading `cd <path> &&` — with
# the session cwd only as the fallback. The previous behavior (always checking
# the session cwd) was wrong in BOTH directions: it blocked feature-branch
# commits made inside a worktree while the session sat on main, and it allowed
# `git -C <main-checkout> commit` while the session sat on a feature branch.
#
# Exit codes:
#   0 - Allow the tool call to proceed
#   2 - Block the tool call (with error message on stderr)

set -euo pipefail

# Protected (default) branch names covered by this hook
readonly PROTECTED_BRANCHES=("main" "master")

# Read the tool call JSON from stdin
INPUT="$(cat)"

# Extract the command from the tool-call JSON. Try python3, then jq, then a
# grep+sed extraction (works without python3/jq), then the raw input. The
# grep+sed tier matters: without it the raw JSON (`{"tool_input":...}`) would
# not be matched by the detection gate, allowing commits/pushes to main.
COMMAND=""
if command -v python3 >/dev/null 2>&1; then
    COMMAND="$(printf '%s' "${INPUT}" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('tool_input',{}).get('command',''))" 2>/dev/null || true)"
elif command -v jq >/dev/null 2>&1; then
    COMMAND="$(printf '%s' "${INPUT}" | jq -r '.tool_input.command // empty' 2>/dev/null || true)"
fi
if [[ -z "${COMMAND}" ]]; then
    COMMAND="$(printf '%s' "${INPUT}" |
        grep -oE '"command"[[:space:]]*:[[:space:]]*"[^"]*"' |
        head -n1 |
        sed -E 's/.*"command"[[:space:]]*:[[:space:]]*"([^"]*)".*/\1/' 2>/dev/null || true)"
fi
if [[ -z "${COMMAND}" ]]; then
    COMMAND="${INPUT}"
fi

# Return true (0) if the given branch name is protected.
is_protected_branch() {
    local branch="${1}"
    local protected
    for protected in "${PROTECTED_BRANCHES[@]}"; do
        if [[ "${branch}" == "${protected}" ]]; then
            return 0
        fi
    done
    return 1
}

# Strip one layer of surrounding single or double quotes from a token.
strip_quotes() {
    local s="${1}"
    s="${s%\"}"
    s="${s#\"}"
    s="${s%\'}"
    s="${s#\'}"
    printf '%s' "${s}"
}

# Resolve the branch of the repository the command actually targets.
# Candidates, in order: `git -C <path>`, `--git-dir[= ]<path>`, a
# `GIT_DIR=<path>` env prefix, a leading `cd <path> &&`/`cd <path>;` —
# each used only when the extracted path is literal (no `$(`/backtick
# substitutions) and exists on disk. Falls back to the session cwd.
# Quoted paths containing spaces are not resolved (fall through to the
# next candidate); the git-level pre-commit hook remains the backstop.
target_branch() {
    local cmd="${1}" path=''

    # git -C <path>
    path="$(printf '%s' "${cmd}" | grep -oE '(^|[[:space:]])git[[:space:]]+-C[[:space:]]+[^[:space:]]+' | head -n1 | sed -E 's/.*-C[[:space:]]+//' || true)"
    path="$(strip_quotes "${path}")"
    if [[ -n "${path}" && "${path}" != *'$('* && "${path}" != *'`'* && -d "${path}" ]]; then
        git -C "${path}" rev-parse --abbrev-ref HEAD 2>/dev/null && return 0
    fi

    # --git-dir=<path> or --git-dir <path>
    path="$(printf '%s' "${cmd}" | grep -oE -- '--git-dir(=|[[:space:]]+)[^[:space:]]+' | head -n1 | sed -E 's/^--git-dir(=|[[:space:]]+)//' || true)"
    path="$(strip_quotes "${path}")"
    if [[ -n "${path}" && "${path}" != *'$('* && "${path}" != *'`'* && -e "${path}" ]]; then
        git --git-dir="${path}" rev-parse --abbrev-ref HEAD 2>/dev/null && return 0
    fi

    # GIT_DIR=<path> environment prefix
    path="$(printf '%s' "${cmd}" | grep -oE 'GIT_DIR=[^[:space:]]+' | head -n1 | sed 's/^GIT_DIR=//' || true)"
    path="$(strip_quotes "${path}")"
    if [[ -n "${path}" && "${path}" != *'$('* && "${path}" != *'`'* && -e "${path}" ]]; then
        git --git-dir="${path}" rev-parse --abbrev-ref HEAD 2>/dev/null && return 0
    fi

    # Leading `cd <path> &&` or `cd <path>;`
    path="$(printf '%s' "${cmd}" | grep -oE '(^|;|&&)[[:space:]]*cd[[:space:]]+[^[:space:];&|]+' | head -n1 | sed -E 's/.*cd[[:space:]]+//' || true)"
    path="$(strip_quotes "${path}")"
    if [[ -n "${path}" && "${path}" != *'$('* && "${path}" != *'`'* && -d "${path}" ]]; then
        git -C "${path}" rev-parse --abbrev-ref HEAD 2>/dev/null && return 0
    fi

    # Fallback: the session cwd
    git rev-parse --abbrev-ref HEAD 2>/dev/null || printf ''
}

# Detection regex: a `git commit`/`git push` command, allowing environment
# assignments (e.g. `GIT_DIR=<path> git commit`) and global git options
# (e.g. `git -C <path> commit`, `git --git-dir=<path> push`) between the
# delimiter and the subcommand, and anchored after the subcommand so
# `git commit-tree` is not matched. The leading delimiter keeps `git` as the
# start of a command (possibly after `;`/`&&`/`|`) rather than text inside
# an argument.
#
# Handles short options that take a value (`-C <path>`, `-c k=v`) and the
# long-form value-taking global options (`--git-dir`, `--work-tree`,
# `--namespace`) in both `=` and space-separated forms. No-argument long
# options (e.g. `--no-pager`) are intentionally not consumed; the commit
# path is backstopped by the git pre-commit hook.
readonly GIT_DELIM='(^|;|&&|\|)'
readonly GIT_ENV='([A-Z_][A-Z_0-9]*=[^[:space:]]*[[:space:]]+)*'
readonly GIT_OPTS='[[:space:]]*'"${GIT_ENV}"'git[[:space:]]+((-[A-Za-z]+[[:space:]]+\S+[[:space:]]+)|(--(git-dir|work-tree|namespace)([[:space:]]*=[[:space:]]*|[[:space:]]+)\S+[[:space:]]+))*'
readonly GIT_TAIL='([[:space:];&|]|$)'
GIT_GATE="${GIT_DELIM}${GIT_OPTS}(commit|push)${GIT_TAIL}"
GIT_COMMIT_GATE="${GIT_DELIM}${GIT_OPTS}commit${GIT_TAIL}"

# If extraction failed and the command is still raw JSON, we cannot reliably
# tokenize refspecs. Fail closed on a protected branch rather than risk
# allowing a commit/push to main through.
if printf '%s' "${COMMAND}" | grep -q '"tool_input"'; then
    BRANCH="$(target_branch '')"
    if is_protected_branch "${BRANCH}"; then
        cat >&2 <<EOF
BLOCKED: Unparseable git command on protected branch '${BRANCH}'.
Per .claude/rules/workflow-never-commit-to-main.md:
  - NEVER commit or push directly to main (or master)
  - All changes to the default branch must go through a pull request
EOF
        exit 2
    fi
    exit 0
fi

# Only inspect git commit / git push commands.
if ! printf '%s' "${COMMAND}" | grep -qE "${GIT_GATE}"; then
    exit 0
fi

# --- git commit: block when the TARGET repository's branch is protected ---
if printf '%s' "${COMMAND}" | grep -qE "${GIT_COMMIT_GATE}"; then
    BRANCH="$(target_branch "${COMMAND}")"
    if is_protected_branch "${BRANCH}"; then
        cat >&2 <<EOF
BLOCKED: git commit on protected branch '${BRANCH}'.
Per .claude/rules/workflow-never-commit-to-main.md:
  - NEVER commit directly to main (or master)
  - All changes to the default branch must go through a pull request
Create a feature branch first:
  git checkout -b fix/issue-<NUMBER>-<slug>
EOF
        exit 2
    fi
    # Do NOT exit here. A commit can be chained with a push
    # (`git commit -m x && git push origin main`); exiting after clearing the
    # commit skipped push analysis entirely and ALLOWED the push (issue #1464).
    # Fall through so every push segment below is still evaluated.
fi

# --- git push: block when any push segment targets a protected branch ---
#
# The refspec scan MUST be bounded to the `git push` command it belongs to.
# Scanning to end-of-line made any later bare `main` look like a refspec, so
# `git push -u origin fix/x && gh pr create --base main` was blocked (issue
# #1464). The command is therefore split on shell control operators and each
# segment evaluated on its own.
#
# Splitting is textual, so a `;`/`&&`/`|` inside a quoted argument splits too.
# That can only split a push segment into smaller pieces — it cannot move a
# protected refspec into a segment that lacks one — so the failure direction is
# over-blocking, never under-blocking.
#
# `git push [<options>] [<repository> [<refspec>...]]`: the first non-option
# arg after `push` is the remote; any further non-option arg is an explicit
# refspec. When an explicit non-protected refspec is given, the current branch
# is irrelevant (git pushes only the named refs), so it must be allowed.
#
# Prints 'refspec' when an explicit protected refspec is present, 'current:<b>'
# when the segment would push a protected current branch, and nothing when the
# segment is safe.
push_segment_verdict() {
    local segment="${1}"
    local arg_count=0 token clean dst saw_push=false branch

    while IFS= read -r token; do
        if [[ "${saw_push}" != "true" ]]; then
            if [[ "${token}" == "push" ]]; then
                saw_push=true
            fi
            continue
        fi
        # Skip option flags (global or push-level)
        if [[ "${token}" == -* ]]; then
            continue
        fi
        arg_count=$((arg_count + 1))
        # Remote URLs (scp-style `user@host:path` or `scheme://host`) are not
        # branch refspecs — skip them so their host/path is not mistaken for one.
        if [[ "${token}" == *"://"* ]] || [[ "${token}" == *"@"* ]]; then
            continue
        fi
        # Strip a leading force-push marker (+); for `src:dst`, examine the dst.
        clean="${token#+}"
        dst="${clean#*:}"
        # Normalize fully-qualified branch refs: refs/heads/main -> main
        dst="${dst#refs/heads/}"
        if is_protected_branch "${dst}"; then
            printf 'refspec'
            return 0
        fi
    done < <(printf '%s\n' "${segment}" | tr -s '[:space:]' '\n')

    # An explicit non-protected refspec was given -> the current branch is not pushed.
    if [[ ${arg_count} -ge 2 ]]; then
        return 0
    fi

    # A tag-only push (no branch refspec) does not move branch refs -> allow it.
    if printf '%s' "${segment}" | grep -qE '(^|[[:space:]])--tags([[:space:]]|$)'; then
        return 0
    fi

    # No explicit branch refspec: git pushes the TARGET repository's current branch.
    branch="$(target_branch "${segment}")"
    if is_protected_branch "${branch}"; then
        printf 'current:%s' "${branch}"
    fi
    return 0
}

# Split on shell control operators (`;`, `&&`, `||`, `|`) — one segment per line.
# `\|\|` is listed first so the two-character operator wins over the single pipe.
while IFS= read -r SEGMENT; do
    [[ -z "${SEGMENT//[[:space:]]/}" ]] && continue
    printf '%s' "${SEGMENT}" | grep -qE "${GIT_DELIM}${GIT_OPTS}push${GIT_TAIL}" || continue

    VERDICT="$(push_segment_verdict "${SEGMENT}")"
    case "${VERDICT}" in
        refspec)
            cat >&2 <<EOF
BLOCKED: git push targets a protected branch (main or master).
Per .claude/rules/workflow-never-commit-to-main.md:
  - NEVER push directly to main (or master)
  - Push a feature branch and open a pull request instead
EOF
            exit 2
            ;;
        current:*)
            cat >&2 <<EOF
BLOCKED: git push would push protected branch '${VERDICT#current:}'.
Per .claude/rules/workflow-never-commit-to-main.md:
  - NEVER push directly to main (or master)
  - Push a feature branch and open a pull request instead
EOF
            exit 2
            ;;
    esac
    # `printf '%s\n'` (not '%s') is required: without the trailing newline `read`
    # returns non-zero on the final segment and the loop body never runs for it —
    # which silently dropped `git push origin main` and let it through.
done < <(printf '%s\n' "${COMMAND}" | sed -E 's/(\|\||&&|;|\|)/\n/g')

exit 0
