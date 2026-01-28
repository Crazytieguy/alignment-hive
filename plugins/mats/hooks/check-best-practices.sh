#!/bin/bash
set -euo pipefail

VERSION_FILE="$CLAUDE_PROJECT_DIR/.claude/mats/best-practices-version"
PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json"
STATE_DIR="$CLAUDE_PROJECT_DIR/.claude/mats"

# Ensure state directory exists so the model can write directly
mkdir -p "$STATE_DIR"

if [ ! -f "$PLUGIN_JSON" ]; then
  exit 0  # Can't check, fail silently
fi
PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')

if [ -z "$PLUGIN_VERSION" ]; then
  exit 0  # Can't parse, fail silently
fi

# Check version file
if [ ! -f "$VERSION_FILE" ]; then
  echo '{"systemMessage": "mats: Best practices available. To set up: /mats:best-practices"}'
  exit 0
fi

CURRENT_VERSION=$(cat "$VERSION_FILE" 2>/dev/null || echo "")

# Extract major.minor (e.g., "0.1.10" → "0.1")
get_minor_version() {
  echo "$1" | cut -d. -f1,2
}

CURRENT_MINOR=$(get_minor_version "$CURRENT_VERSION")
PLUGIN_MINOR=$(get_minor_version "$PLUGIN_VERSION")

# Only prompt on minor version bumps, not patch bumps
if [ "$CURRENT_MINOR" != "$PLUGIN_MINOR" ]; then
  echo '{"systemMessage": "mats: New best practices available. To review: /mats:best-practices"}'
fi

exit 0
