#!/bin/bash
set -euo pipefail

# Auto-allow:
# 1. deno-sandbox-grant help/no-args (read-only)
# 2. Read/Write/Edit to registered sandbox script files
# 3. Read/Grep/Glob on directories granted via deno-sandbox-grant
#
# deno-sandbox execution is handled by the Bash(deno-sandbox *) permission rule
# added during /autopilot:setup — not validated here.

input=$(cat)

# shellcheck source=../scripts/find-jq.sh
source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || exit 0

if ! "$JQ" -e '.deno_sandbox == true' "$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json" >/dev/null 2>&1; then
  exit 0
fi

eval "$( echo "$input" | "$JQ" -r '
  "tool_name=" + (.tool_name // "" | @sh),
  "target_path=" + (.tool_input.file_path // .tool_input.path // "" | @sh),
  "session_id=" + (.session_id // "" | @sh),
  "bash_command=" + (.tool_input.command // "" | @sh)
')"

sandbox_dir="${CLAUDE_PROJECT_DIR}/.claude/deno-sandbox"
# Without a session id nothing below can match, and the grants path would degenerate.
[ -n "$session_id" ] || exit 0
sessions_dir="${AUTOPILOT_DATA_DIR:-${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}}/sessions"

# --- Helpers ---

emit_allow() {
  "$JQ" -n '{ hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } } }'
}

emit_deny() {
  "$JQ" -n --arg msg "$1" '{ hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "deny", message: $msg } } }'
}

# --- Bash tool ---

if [ "$tool_name" = "Bash" ]; then
  # Auto-allow deno-sandbox-grant with no args or --help (read-only)
  case "$bash_command" in
    deno-sandbox-grant|"deno-sandbox-grant --help"|"deno-sandbox-grant -h")
      emit_allow
      exit 0 ;;
  esac
  exit 0
fi

# --- File tools ---

# Only relevant for Write, Edit, Read, Grep, Glob
case "${tool_name:-}" in
  Write|Edit|Read|Grep|Glob) ;;
  *) exit 0 ;;
esac

if [ -z "$target_path" ]; then
  exit 0
fi

# Reject paths with traversal
if [[ "$target_path" == *..* ]]; then
  exit 0
fi

# Auto-allow Read/Write/Edit to this session's and its registered agents' sandbox scripts
ids=("$session_id")
if [ -f "$sessions_dir/$session_id.agents" ]; then
  while IFS= read -r id; do [ -n "$id" ] && ids+=("$id"); done < "$sessions_dir/$session_id.agents"
fi
for id in "${ids[@]}"; do
  if [ "$target_path" = "$sandbox_dir/$id.ts" ]; then
    emit_allow
    exit 0
  fi
done

# Deny writes to unregistered .ts files in the sandbox dir
if [ "$tool_name" = "Write" ] || [ "$tool_name" = "Edit" ]; then
  if [[ "$target_path" == */.claude/deno-sandbox/* ]]; then
    # Only block writes to script files (.ts), not config/declaration files (.d.ts, .json, etc.)
    case "$target_path" in
      *.d.ts) ;;
      *.ts) emit_deny "Write to your assigned sandbox script file instead." ;;
    esac
  fi
  exit 0
fi

# Load granted read paths from session state
STATE_FILE="$sessions_dir/$session_id"
if [ ! -f "$STATE_FILE" ]; then
  exit 0
fi

read_paths=()
while IFS= read -r line; do
  if [[ "$line" =~ ^--allow-read=(.+)$ ]]; then
    read_paths+=("${BASH_REMATCH[1]}")
  fi
done < "$STATE_FILE"

if [ ${#read_paths[@]} -eq 0 ]; then
  exit 0
fi

# Resolve target to absolute path
if [[ "$target_path" != /* ]]; then
  target_path="$(pwd)/$target_path"
fi

# Check if target falls under any granted read path (already absolute from deno-sandbox-grant)
for granted in "${read_paths[@]}"; do
  if [[ "$target_path" == "$granted" || "$target_path" == "$granted/"* ]]; then
    emit_allow
    exit 0
  fi
done

exit 0
