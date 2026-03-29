#!/bin/bash
set -euo pipefail

# SubagentStart hook: inject deno-sandbox instructions and register agent ID.
# Fires for all subagents; exits early if sandbox is disabled or deno unavailable.

STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"

# shellcheck source=../scripts/find-jq.sh
source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || exit 0

# Check sandbox is enabled
if ! "$JQ" -e '.deno_sandbox == true' "$STATE_FILE" >/dev/null 2>&1; then
  exit 0
fi

# Check deno is available
if ! command -v deno >/dev/null 2>&1 && [ ! -x "$HOME/.deno/bin/deno" ]; then
  exit 0
fi

# Parse hook input
hook_input=$(cat)
session_id=$(echo "$hook_input" | "$JQ" -r '.session_id // ""')
agent_id=$(echo "$hook_input" | "$JQ" -r '.agent_id // ""')

if [ -z "$session_id" ] || [ -z "$agent_id" ]; then
  exit 0
fi

# Validate agent_id for safe filename characters
if [[ "$agent_id" =~ [/\\] ]]; then
  exit 0
fi

# Register agent ID for permission validation
registry_dir="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/sessions"
mkdir -p "$registry_dir"
registry_file="$registry_dir/$session_id.agents"
if ! grep -qxF -- "$agent_id" "$registry_file" 2>/dev/null; then
  echo "$agent_id" >> "$registry_file"
fi

# Emit sandbox instructions
SANDBOX_DIR="$CLAUDE_PROJECT_DIR/.claude/deno-sandbox"
sandbox_script="$SANDBOX_DIR/$agent_id.ts"
# shellcheck source=../scripts/sandbox-instructions.sh
additional_context=$(source "${CLAUDE_PLUGIN_ROOT}/scripts/sandbox-instructions.sh" "$sandbox_script" "$SANDBOX_DIR")

"$JQ" -n --arg ctx "$additional_context" '{
  hookSpecificOutput: {
    hookEventName: "SubagentStart",
    additionalContext: $ctx
  }
}'
