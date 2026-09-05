#!/bin/bash
set -euo pipefail

# SessionStart hook for the hive plugin: minimal bash that records session state and
# delegates to the binary. Prefer exiting 0 — a non-zero exit shows a warning to the user.

source "${CLAUDE_PLUGIN_ROOT}/scripts/common.sh"

HOOK_INPUT=$(cat)

STATE_DIR="$(resolve_state_dir "$CLAUDE_PROJECT_DIR")"
ERROR_LOG="$STATE_DIR/error.log"

# Exit 0 on unexpected errors — log them for debugging
trap 'echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] session-start.sh: unexpected error at line $LINENO" >> "$ERROR_LOG" 2>/dev/null; exit 0' ERR

mkdir -p "$STATE_DIR"
[ -f "$STATE_DIR/.gitignore" ] || echo '*' > "$STATE_DIR/.gitignore"

SOURCE=$(echo "$HOOK_INPUT" | jq -r '.source // "startup"')
SESSION_ID=$(echo "$HOOK_INPUT" | jq -r '.session_id // ""')
TRANSCRIPT_PATH=$(echo "$HOOK_INPUT" | jq -r '.transcript_path // ""')

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
# checkout) still needs its transcript dir registered.
# Derived from transcript_path rather than recomputing Claude Code's
# project-dir naming (which sanitizes and truncates; see toClaudeProjectDirName).

if [ -n "$TRANSCRIPT_PATH" ]; then
  TRANSCRIPT_DIR=$(dirname "$TRANSCRIPT_PATH")
  if [ -d "$TRANSCRIPT_DIR" ]; then
    TRANSCRIPTS_FILE="$STATE_DIR/transcripts-dirs"
    if ! grep -qxF "$TRANSCRIPT_DIR" "$TRANSCRIPTS_FILE" 2>/dev/null; then
      echo "$TRANSCRIPT_DIR" >> "$TRANSCRIPTS_FILE"
    fi
  fi
fi

# --- Skip for resume/compact/fork (continuations don't need fresh state) ---

if [ "$SOURCE" = "resume" ] || [ "$SOURCE" = "compact" ] || [ "$SOURCE" = "fork" ]; then
  exit 0
fi

# --- Delegate to binary (handles version check, consent, uploads) ---

export HIVE_PLUGIN_VERSION="$(plugin_version "$CLAUDE_PLUGIN_ROOT")"

# Dev binary shortcircuit: use the locally-built binary when running from the repo
if [[ "$CLAUDE_PLUGIN_ROOT" == "${CLAUDE_PROJECT_DIR}"/* ]] && [ -x "$CLAUDE_PROJECT_DIR/.dev/hive" ]; then
  HIVE="$CLAUDE_PROJECT_DIR/.dev/hive"
else
  # Bootstrap: ensure the correct CLI version is cached, updated, and exec'd
  HIVE="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"
fi
echo "$HOOK_INPUT" | "$HIVE" session-start 2>>"$ERROR_LOG" || true
