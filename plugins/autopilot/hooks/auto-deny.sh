#!/bin/bash

input=$(cat)

LOG_FILE="$HOME/.cache/autopilot/auto-deny-error.log"
mkdir -p "$(dirname "$LOG_FILE")"

# On any error, log it and tell the user
trap 'echo "$0: line $LINENO: unexpected error" >> "$LOG_FILE" 2>/dev/null; echo "{\"systemMessage\":\"\u001b[1;32mautopilot:\u001b[0m hook error, autonomous mode disabled, see '$LOG_FILE'\"}"' ERR
set -euo pipefail

# shellcheck source=../scripts/find-jq.sh
source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || exit 0

# Extract everything we need in a single jq call
# Use .cwd from hook input (not $CLAUDE_PROJECT_DIR) — .cwd tracks the actual
# working directory, which differs from the project dir inside worktrees.
eval "$(echo "$input" | "$JQ" -r '.cwd as $cwd |
  "permission_mode=" + (.permission_mode | @sh),
  "has_session_dest=" + ([.permission_suggestions // [] | .[] | select(.destination == "session")] | length | tostring),
  "session_in_cwd=" + ([.permission_suggestions // [] | .[] | select(.destination == "session") | .rules[]? | .ruleContent | gsub("^/+"; "/") | gsub("/\\*\\*$"; "") | startswith($cwd)] | any | tostring),
  "rule_content=" + ([.permission_suggestions // [] | .[] | select(.type == "addRules" and .destination != "session") | .rules[]? | .ruleContent] | first // "" | @sh),
  "has_suggestions=" + (.permission_suggestions // [] | length > 0 | tostring)
')"

if [ "$permission_mode" != "acceptEdits" ]; then
  exit 0
fi

# Check state file
STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"
if ! "$JQ" -e '.autonomous_mode == true' "$STATE_FILE" >/dev/null 2>&1; then
  exit 0
fi

# Let through session suggestions unless they point inside the cwd (bogus)
if [ "$has_session_dest" -gt 0 ] && [ "$session_in_cwd" != "true" ]; then
  exit 0
fi

# Let deno-sandbox and deno-sandbox-grant through (sandbox-permissions.sh validates paths)
if [[ "${rule_content%% *}" == "deno-sandbox-grant" ]] || [[ "${rule_content%% *}" == "deno-sandbox" ]]; then
  exit 0
fi

# Build context-aware deny message
if [ -n "$rule_content" ]; then
  message="Command denied in autonomous mode. \`${rule_content}\` is not in the allow list. Consider /autopilot:resolve-denied-toolcall."
elif [ "$has_suggestions" = "true" ]; then
  message="Command denied in autonomous mode. Command likely contains variable references or field expressions that conflict with permission matching."
else
  message="Command denied in autonomous mode. Command likely contains command substitution or ambiguous syntax that conflicts with permission matching."
fi

# Output deny decision using jq to ensure valid JSON
"$JQ" -n --arg msg "$message" '{
  hookSpecificOutput: {
    hookEventName: "PermissionRequest",
    decision: {
      behavior: "deny",
      message: $msg,
      interrupt: false
    }
  }
}'
