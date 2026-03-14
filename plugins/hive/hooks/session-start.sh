#!/bin/bash
set -euo pipefail

# SessionStart hook for the hive plugin.
# Minimal bash: sets up env, registers transcript dir, delegates to binary.

PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json"

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

# --- Dev environment setup via CLAUDE_ENV_FILE ---

if [ -n "${CLAUDE_ENV_FILE:-}" ] && [ -f "$CLAUDE_PROJECT_DIR/dev-env.sh" ]; then
  echo "export PATH=\"$CLAUDE_PROJECT_DIR/.dev:\$PATH\"" >> "$CLAUDE_ENV_FILE"
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

# Find hive binary: PATH, ~/.local/bin, or .dev/ (dev mode)
HIVE_BIN=""
if command -v hive >/dev/null 2>&1; then
  HIVE_BIN="hive"
elif [ -x "$HOME/.local/bin/hive" ]; then
  HIVE_BIN="$HOME/.local/bin/hive"
elif [ -x "$CLAUDE_PROJECT_DIR/.dev/hive" ]; then
  HIVE_BIN="$CLAUDE_PROJECT_DIR/.dev/hive"
fi

if [ -n "$HIVE_BIN" ]; then
  # Pass plugin version so binary can do the version check
  PLUGIN_VERSION=""
  if [ -f "$PLUGIN_JSON" ]; then
    PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
  fi

  ERROR_LOG="$STATE_DIR/error.log"
  export HIVE_PLUGIN_VERSION="$PLUGIN_VERSION"
  "$HIVE_BIN" session-start 2>>"$ERROR_LOG" || true
fi

exit 0
