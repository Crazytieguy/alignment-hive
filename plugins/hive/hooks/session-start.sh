#!/bin/bash
set -euo pipefail

# Merged SessionStart hook for the hive plugin.
# 1. Version check for /hive:align (stays in bash, fast)
# 2. Session sharing via hive-cli binary (only if consent file exists)

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

# --- Part 1: Version check for /hive:align ---

if [ -f "$PLUGIN_JSON" ]; then
  PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')

  if [ -n "$PLUGIN_VERSION" ]; then
    VERSION_FILE="$STATE_DIR/align-version"

    if [ ! -f "$VERSION_FILE" ]; then
      echo '{"systemMessage": "hive: Tooling recommendations available. To set up: /hive:align"}'
    else
      CURRENT_VERSION=$(cat "$VERSION_FILE" 2>/dev/null || echo "")

      # Extract major.minor
      CURRENT_MINOR=$(echo "$CURRENT_VERSION" | cut -d. -f1,2)
      PLUGIN_MINOR=$(echo "$PLUGIN_VERSION" | cut -d. -f1,2)

      # Only prompt on minor version bumps
      if [ "$CURRENT_MINOR" != "$PLUGIN_MINOR" ]; then
        echo '{"systemMessage": "hive: New recommendations available. To review: /hive:align"}'
      fi
    fi
  fi
fi

# --- Part 1.5: Dev environment setup via CLAUDE_ENV_FILE ---

if [ -n "${CLAUDE_ENV_FILE:-}" ] && [ -f "$CLAUDE_PROJECT_DIR/dev-env.sh" ]; then
  # Resolve paths at hook time — CLAUDE_PROJECT_DIR and BASH_SOURCE won't be
  # available when Claude Code sources the env file later
  echo "export ALIGNMENT_HIVE_DEV=1" >> "$CLAUDE_ENV_FILE"
  echo "export PATH=\"$CLAUDE_PROJECT_DIR/.dev:\$PATH\"" >> "$CLAUDE_ENV_FILE"
fi

# --- Part 2: Register transcript directory for local retrieval (always) ---

TRANSCRIPT_DIR="$HOME/.claude/projects/$(echo "$CLAUDE_PROJECT_DIR" | tr '/' '-')"
if [ -d "$TRANSCRIPT_DIR" ]; then
  TRANSCRIPTS_FILE="$STATE_DIR/transcripts-dirs"
  if ! grep -qxF "$TRANSCRIPT_DIR" "$TRANSCRIPTS_FILE" 2>/dev/null; then
    echo "$TRANSCRIPT_DIR" >> "$TRANSCRIPTS_FILE"
  fi
fi

# --- Part 3: Session sharing (only if consent file exists) ---

CONSENT_FILE="$STATE_DIR/sharing-enabled"

if [ -f "$CONSENT_FILE" ]; then
  # Invoke bootstrap.sh which downloads binary if needed, then runs session-start
  # Pipe stdin through so the binary gets hook input
  # Errors go to log file; hook must not fail even if bootstrap/binary fails
  ERROR_LOG="$STATE_DIR/error.log"
  bash "${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh" session-start 2>>"$ERROR_LOG" || true
fi

exit 0
