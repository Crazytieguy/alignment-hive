#!/bin/bash
set -euo pipefail

# Auto-allow:
# 1. deno-sandbox-grant help/no-args (read-only)
# 2. Read/Write/Edit to the session's sandbox script file
# 3. Read/Grep/Glob on directories granted via deno-sandbox-grant

input=$(cat)

# Find jq
if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "$HOME/.cache/autopilot/jq" ]; then
  JQ="$HOME/.cache/autopilot/jq"
else
  exit 0
fi

eval "$( echo "$input" | "$JQ" -r '
  "tool_name=" + (.tool_name // "" | @sh),
  "target_path=" + (.tool_input.file_path // .tool_input.path // "" | @sh),
  "session_id=" + (.session_id // "" | @sh)
')"

# Auto-allow deno-sandbox-grant with no args or --help (read-only)
if [ "$tool_name" = "Bash" ]; then
  command=$(echo "$input" | "$JQ" -r '.tool_input.command // ""')
  case "$command" in
    deno-sandbox-grant|"deno-sandbox-grant --help"|"deno-sandbox-grant -h")
      "$JQ" -n '{
        hookSpecificOutput: {
          hookEventName: "PermissionRequest",
          decision: { behavior: "allow" }
        }
      }' ;;
  esac
  exit 0
fi

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

# Auto-allow Read/Write/Edit to the sandbox script file, deny writes to other sandbox files
sandbox_dir="${CLAUDE_PROJECT_DIR}/.claude/deno-sandbox"
sandbox_script="$sandbox_dir/${session_id}.ts"
if [ -n "$session_id" ] && [ "$target_path" = "$sandbox_script" ]; then
  "$JQ" -n '{
    hookSpecificOutput: {
      hookEventName: "PermissionRequest",
      decision: { behavior: "allow" }
    }
  }'
  exit 0
fi
if [ "$tool_name" = "Write" ] || [ "$tool_name" = "Edit" ]; then
  if [[ "$target_path" == "$sandbox_dir"/* ]] || [[ "$target_path" == */.claude/deno-sandbox/* ]]; then
    "$JQ" -n --arg script "$sandbox_script" '{
      hookSpecificOutput: {
        hookEventName: "PermissionRequest",
        decision: {
          behavior: "deny",
          message: ("Write to your sandbox script file instead: " + $script)
        }
      }
    }'
  fi
  exit 0
fi

# Load granted read paths from session state
STATE_FILE="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/sessions/${session_id}"
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
    "$JQ" -n --arg tool "$tool_name" --arg path "$granted" '{
      hookSpecificOutput: {
        hookEventName: "PermissionRequest",
        decision: {
          behavior: "allow",
          updatedPermissions: [
            {
              type: "addRules",
              rules: [{ toolName: $tool, ruleContent: ($path + "/**") }],
              behavior: "allow",
              destination: "session"
            }
          ]
        }
      }
    }'
    exit 0
  fi
done

exit 0
