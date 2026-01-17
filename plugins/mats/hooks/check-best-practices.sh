#!/bin/bash
set -euo pipefail

VERSION_FILE="$CLAUDE_PROJECT_DIR/.claude/mats:best-practices-version"
PLUGIN_JSON="${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json"

if [ ! -f "$PLUGIN_JSON" ]; then
  exit 0  # Can't check, fail silently
fi
PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')

if [ -z "$PLUGIN_VERSION" ]; then
  exit 0  # Can't parse, fail silently
fi

# Check version file
if [ ! -f "$VERSION_FILE" ]; then
  echo '{"systemMessage": "Best practices not yet reviewed. To set up: /mats:best-practices"}'
  exit 0
fi

CURRENT_VERSION=$(cat "$VERSION_FILE" 2>/dev/null || echo "")

if [ "$CURRENT_VERSION" != "$PLUGIN_VERSION" ]; then
  echo '{"systemMessage": "New best practices available. To review: /mats:best-practices"}'
fi

exit 0
