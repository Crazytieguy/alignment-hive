#!/bin/bash
# Auto-configure worktrees and ensure dependencies on session start

MAIN_WORKTREE=$(git worktree list --porcelain 2>/dev/null | head -1 | sed 's/worktree //')
CURRENT_DIR=$(pwd)
MESSAGES=""

# Ensure git hooks are configured
if [ -d "$CURRENT_DIR/.githooks" ]; then
  git config core.hooksPath .githooks
fi

# Symlink .env.local if in a worktree (not main)
if [ -n "$MAIN_WORKTREE" ] && [ "$MAIN_WORKTREE" != "$CURRENT_DIR" ]; then
  ENV_SOURCE="$MAIN_WORKTREE/hive-mind/.env.local"
  ENV_TARGET="$CURRENT_DIR/hive-mind/.env.local"

  if [ -f "$ENV_SOURCE" ] && [ ! -e "$ENV_TARGET" ]; then
    ln -sf "$ENV_SOURCE" "$ENV_TARGET"
    MESSAGES="Symlinked .env.local"
  fi
fi

# Always ensure dependencies are installed
if [ -d "$CURRENT_DIR/hive-mind" ]; then
  cd "$CURRENT_DIR/hive-mind" && bun install --silent
fi

if [ -n "$MESSAGES" ]; then
  echo "{\"systemMessage\": \"$MESSAGES\"}"
fi
