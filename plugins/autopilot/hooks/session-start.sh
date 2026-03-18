#!/bin/bash
set -euo pipefail

STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"
LOG_FILE="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/bootstrap.log"

# On any error, log and inform user
mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null
trap 'echo "$0: line $LINENO: unexpected error" >> "$LOG_FILE" 2>/dev/null; echo "{\"systemMessage\":\"autopilot: Session start error. Details: $LOG_FILE\"}"' ERR

# No state file → not configured
if [ ! -f "$STATE_FILE" ]; then
  echo '{"systemMessage": "autopilot: Not configured. Run /autopilot:setup"}'
  exit 0
fi

# Find jq — check only, no download yet
if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/jq" ]; then
  JQ="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/jq"
else
  # No jq yet — async hook will bootstrap it and report back
  echo '{"systemMessage": "autopilot: Bootstrapping jq. Auto-deny will activate once ready."}'
  exit 0
fi

# Check autonomous mode
if "$JQ" -e '.autonomous_mode == true' "$STATE_FILE" >/dev/null 2>&1; then
  echo '{"systemMessage": "autopilot: Autonomous mode active"}'
else
  echo '{"systemMessage": "autopilot: Installed"}'
fi
