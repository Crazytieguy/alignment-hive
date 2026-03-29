#!/bin/bash
# Sourceable snippet that sets JQ to a jq binary path.
# Returns 1 if jq is not available (caller decides how to handle).
# Usage: source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || exit 0

if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "$HOME/.cache/autopilot/jq" ]; then
  JQ="$HOME/.cache/autopilot/jq"
else
  return 1
fi
