#!/bin/bash
set -euo pipefail

# SessionStart hook for the hive plugin.
# Minimal bash: sets up env, registers transcript dir, delegates to binary.
# Prefer exiting 0 — a non-zero exit just shows a small warning message to the user.
#
# Only runs full logic on "startup" and "clear" (fresh context).
# Skips "resume" and "compact" (continuations where state is already recorded).

PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json"

# --- Parse hook input from stdin ---

HOOK_INPUT=$(cat)
SOURCE=$(echo "$HOOK_INPUT" | jq -r '.source // "startup"')
SESSION_ID=$(echo "$HOOK_INPUT" | jq -r '.session_id // ""')

# --- Resolve main worktree path for state dir ---

resolve_state_dir() {
  local main_worktree
  main_worktree=$(git worktree list --porcelain 2>/dev/null | head -1 | sed 's/^worktree //' || echo "")
  if [ -z "$main_worktree" ]; then
    main_worktree="$CLAUDE_PROJECT_DIR"
  fi
  echo "$main_worktree/.claude/hive"
}

STATE_DIR="$(resolve_state_dir)"

# Ensure state directory exists
mkdir -p "$STATE_DIR"

ERROR_LOG="$STATE_DIR/error.log"

# Exit 0 on unexpected errors — log them for debugging
trap 'echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] session-start.sh: unexpected error at line $LINENO" >> "$ERROR_LOG" 2>/dev/null; exit 0' ERR

# --- Skip for resume/compact (continuations don't need fresh state) ---

if [ "$SOURCE" = "resume" ] || [ "$SOURCE" = "compact" ]; then
  exit 0
fi

# --- Record git commit hash at session start ---

if [ -n "$SESSION_ID" ]; then
  COMMIT_HASH=$(git rev-parse HEAD 2>/dev/null || echo "")
  if [ -n "$COMMIT_HASH" ]; then
    echo "$COMMIT_HASH" > "$STATE_DIR/${SESSION_ID}-commit.txt"
  fi
fi

# --- Register transcript directory for local retrieval ---

TRANSCRIPT_DIR="$HOME/.claude/projects/$(echo "$CLAUDE_PROJECT_DIR" | tr '/' '-')"
if [ -d "$TRANSCRIPT_DIR" ]; then
  TRANSCRIPTS_FILE="$STATE_DIR/transcripts-dirs"
  if ! grep -qxF "$TRANSCRIPT_DIR" "$TRANSCRIPTS_FILE" 2>/dev/null; then
    echo "$TRANSCRIPT_DIR" >> "$TRANSCRIPTS_FILE"
  fi
fi

# --- Delegate to binary (handles version check, consent, uploads) ---

DISABLE_FILE="$STATE_DIR/sharing-disabled"

if [ -f "$DISABLE_FILE" ]; then
  exit 0
fi

# Find hive binary: dev binary when running from local plugin dir, else production
HIVE_BIN=""
if [[ "${CLAUDE_PLUGIN_ROOT:-}" == "${CLAUDE_PROJECT_DIR}"/* ]] && [ -x "$CLAUDE_PROJECT_DIR/.dev/hive" ]; then
  HIVE_BIN="$CLAUDE_PROJECT_DIR/.dev/hive"
elif command -v hive >/dev/null 2>&1; then
  HIVE_BIN="hive"
elif [ -x "$HOME/.local/bin/hive" ]; then
  HIVE_BIN="$HOME/.local/bin/hive"
fi

if [ -n "$HIVE_BIN" ]; then
  # Pass plugin version so binary can do the version check
  PLUGIN_VERSION=""
  if [ -f "$PLUGIN_JSON" ]; then
    PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' || echo "")
  fi

  export HIVE_PLUGIN_VERSION="$PLUGIN_VERSION"
  echo "$HOOK_INPUT" | "$HIVE_BIN" session-start 2>>"$ERROR_LOG" || true
fi

exit 0
