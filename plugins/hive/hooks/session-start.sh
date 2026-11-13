#!/bin/bash
# SessionStart hook for the hive plugin: minimal bash that records session state and
# delegates to the binary. Prefer exiting 0 — a non-zero exit shows a warning to the user.
set -euo pipefail

source "${CLAUDE_PLUGIN_ROOT}/scripts/common.sh"

HOOK_INPUT=$(cat)

STATE_DIR="$(resolve_state_dir "$CLAUDE_PROJECT_DIR")"
ERROR_LOG="$STATE_DIR/error.log"

# Exit 0 on unexpected errors — log them for debugging
trap 'echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] session-start.sh: unexpected error at line $LINENO" >> "$ERROR_LOG" 2>/dev/null; exit 0' ERR

mkdir -p "$STATE_DIR"
[ -f "$STATE_DIR/.gitignore" ] || echo '*' > "$STATE_DIR/.gitignore"

SOURCE=$(echo "$HOOK_INPUT" | "$JQ" -r '.source // "startup"')
SESSION_ID=$(echo "$HOOK_INPUT" | "$JQ" -r '.session_id // ""')
TRANSCRIPT_PATH=$(echo "$HOOK_INPUT" | "$JQ" -r '.transcript_path // ""')

# --- Record git commit hash for this session id ---
# Runs before the resume/compact/fork early-exit: a forked session has a new
# session id that still needs its commit stamp. Forks report source:"fork" on
# Claude Code >= 2.1.214 and source:"resume" on older versions; both are
# skipped by the early-exit below. Same-id resumes keep their original hash
# via the file-existence guard.

if [ -n "$SESSION_ID" ] && [ ! -f "$STATE_DIR/${SESSION_ID}-commit.txt" ]; then
  COMMIT_HASH=$(git rev-parse HEAD 2>/dev/null || echo "")
  if [ -n "$COMMIT_HASH" ]; then
    echo "$COMMIT_HASH" > "$STATE_DIR/${SESSION_ID}-commit.txt"
  fi
fi

# --- Register transcript directory for local retrieval ---
# Also runs before the early-exit: a fork/resume in a directory that never had
# a plain startup (e.g. `claude --resume <id> --fork-session` from a fresh
# checkout) still needs its transcript dir registered. Mid-session moves
# (EnterWorktree/ExitWorktree) fire no SessionStart; the PostToolUse hook
# register-transcript-dir.sh covers those with the same function.

register_transcript_dir "$STATE_DIR" "$TRANSCRIPT_PATH"

# --- Recall's memory guidance, on every source ---
# The module's system-prompt section (hooks/register.tsx) reads the same file, but a policy such as
# Claude Code's sec-default drops plugins' system-prompt sections and keeps hook context. A
# compaction drops what an earlier start added, so it goes out again; on a resume Claude Code
# leaves out what the conversation already holds.

# With the tool on, it holds only while the tool is listed: a policy can refuse the module that
# registers it and still run this hook. Automatic recall alone leaves Claude no such sign.

if [ "${CLAUDE_PLUGIN_OPTION_RECALL:-}" = "true" ]; then
  printf '%s %s %s\n' \
    'The user turned on recall (the hive plugin): while the `mcp__hive__recall` tool is in your tool list, it reads the user'"'"'s messages and your replies from this project'"'"'s recent sessions.' \
    "$(cat "${CLAUDE_PLUGIN_ROOT}/hooks/memory-guidance.txt")" \
    'If that tool is not in your tool list, recall is not running in this session and none of this applies.'
elif [ "${CLAUDE_PLUGIN_OPTION_AUTOMATICRECALL:-}" = "true" ]; then
  printf '%s %s\n' \
    'The user turned on automatic recall (the hive plugin): before each prompt the user sends, it reads the user'"'"'s messages and your replies from this project'"'"'s recent sessions and attaches the entries that bear on the prompt, as a `[hive recall]` block.' \
    "$(cat "${CLAUDE_PLUGIN_ROOT}/hooks/memory-guidance.txt")"
fi

# --- Skip for resume/compact/fork (continuations don't need fresh state) ---

if [ "$SOURCE" = "resume" ] || [ "$SOURCE" = "compact" ] || [ "$SOURCE" = "fork" ]; then
  exit 0
fi

# --- Delegate to binary (handles version check, consent, uploads) ---

# Silent: what the person should know shows in the plugin's band (hooks/register.tsx), and
# bootstrap.sh's not-installed and update-failed lines are band rows too.
echo "$HOOK_INPUT" | bash "${CLAUDE_PLUGIN_ROOT}/scripts/hive.sh" session-start >/dev/null 2>>"$ERROR_LOG" || true
