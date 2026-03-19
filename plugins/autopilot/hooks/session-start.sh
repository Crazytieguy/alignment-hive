#!/bin/bash
set -euo pipefail

STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"
LOG_FILE="$HOME/.cache/autopilot/bootstrap.log"

# ANSI via JSON unicode escapes
B='\u001b[1m'   # bold
M='\u001b[1;35m' # bold magenta
R='\u001b[0m'    # reset

# On any error, log and inform user
mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null
trap 'echo "$0: line $LINENO: unexpected error" >> "$LOG_FILE" 2>/dev/null; echo "{\"systemMessage\":\"${B}autopilot:${R} session start error, see $LOG_FILE\"}"' ERR

# No state file → not configured
if [ ! -f "$STATE_FILE" ]; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} not configured, run ${M}/autopilot:setup${R}\"}"
  exit 0
fi

# Find jq — check only, no download yet
if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "$HOME/.cache/autopilot/jq" ]; then
  JQ="$HOME/.cache/autopilot/jq"
else
  # No jq yet — async hook will bootstrap it and report back
  echo "{\"systemMessage\": \"${B}autopilot:${R} bootstrapping jq, auto-deny will activate once ready\"}"
  exit 0
fi

# Check autonomous mode
if "$JQ" -e '.autonomous_mode == true' "$STATE_FILE" >/dev/null 2>&1; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} autonomous mode active\"}"
else
  echo "{\"systemMessage\": \"${B}autopilot:${R} installed\"}"
fi
