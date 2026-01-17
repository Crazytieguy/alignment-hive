#!/bin/bash
# Outputs status info for the best-practices command

# Get plugin root from script location (script is in plugins/mats/scripts/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="${1:-$(dirname "$SCRIPT_DIR")}"
PROJECT_DIR="${2:-$PWD}"

# Get plugin version
PLUGIN_JSON="$PLUGIN_ROOT/.claude-plugin/plugin.json"
if [ -f "$PLUGIN_JSON" ]; then
  PLUGIN_VERSION=$(grep '"version"' "$PLUGIN_JSON" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
else
  PLUGIN_VERSION="unknown"
fi

# Get last run version
VERSION_FILE="$PROJECT_DIR/.claude/mats/best-practices-version"
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
