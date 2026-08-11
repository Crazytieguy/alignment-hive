#!/bin/bash
set -euo pipefail
# Outputs status info for the align command

# Get plugin root from script location (script is in plugins/hive/scripts/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="${1:-$(dirname "$SCRIPT_DIR")}"
PROJECT_DIR="${2:-$PWD}"

# Resolve main worktree path for state dir
resolve_state_dir() {
  local main_worktree
  main_worktree=$(git worktree list --porcelain 2>/dev/null | head -1 | sed 's/^worktree //' || echo "")
  if [ -z "$main_worktree" ]; then
    main_worktree="$PROJECT_DIR"
  fi
  echo "$main_worktree/.claude/hive"
}

STATE_DIR="$(resolve_state_dir)"

# Get plugin version
PLUGIN_JSON="$PLUGIN_ROOT/.claude-plugin/plugin.json"
if [ -f "$PLUGIN_JSON" ]; then
  PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' || echo "unknown")
else
  PLUGIN_VERSION="unknown"
fi

# Get last run version
VERSION_FILE="$STATE_DIR/align-version"
if [ -f "$VERSION_FILE" ]; then
  LAST_VERSION=$(cat "$VERSION_FILE")
  RUN_TYPE="follow-up"
else
  LAST_VERSION="never run"
  RUN_TYPE="first-time"
fi

echo "**Plugin version**: $PLUGIN_VERSION"
echo "**Last run version**: $LAST_VERSION"
echo "**Run type**: $RUN_TYPE"

# Platform-specific marketplace entries for the plugins that ship a binary.
# Resolved here rather than in the command prose so the target triple has one
# definition and the catalog lookup is exact.
case "$(uname -s 2>/dev/null)" in
  Linux) OS_TRIPLE="unknown-linux-gnu" ;;
  Darwin) OS_TRIPLE="apple-darwin" ;;
  *) OS_TRIPLE="" ;;
esac
case "$(uname -m 2>/dev/null)" in
  x86_64) ARCH_TRIPLE="x86_64" ;;
  aarch64 | arm64) ARCH_TRIPLE="aarch64" ;;
  *) ARCH_TRIPLE="" ;;
esac

CATALOG="$HOME/.claude/plugins/marketplaces/alignment-hive/.claude-plugin/marketplace.json"
AVAILABLE=""
if [ -n "$OS_TRIPLE" ] && [ -n "$ARCH_TRIPLE" ]; then
  SUFFIX="-${ARCH_TRIPLE}-${OS_TRIPLE}"
  for plugin in model-router remote-kernels; do
    if grep -q "\"${plugin}${SUFFIX}\"" "$CATALOG" 2>/dev/null; then
      AVAILABLE="$AVAILABLE $plugin"
    fi
  done
else
  SUFFIX="none (unsupported platform)"
fi

echo "**Platform entry suffix**: $SUFFIX"
echo "**Platform entries available for**:${AVAILABLE:- none}"
