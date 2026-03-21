#!/bin/bash
set -euo pipefail

# Auto-allow:
# 1. Write/Edit to .claude/deno-sandbox/ (sandbox script files)
# 2. Read/Grep/Glob on directories granted via deno-sandbox-grant

input=$(cat)

# Quick pre-filter: only relevant for Write, Edit, Read, Grep, Glob
tool_name=$(printf '%s' "$input" | grep -o '"tool_name":"[^"]*"' | head -1 | cut -d'"' -f4)
case "${tool_name:-}" in
  Write|Edit|Read|Grep|Glob) ;;
  *) exit 0 ;;
esac

# Find jq
if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "$HOME/.cache/autopilot/jq" ]; then
  JQ="$HOME/.cache/autopilot/jq"
else
  exit 0
fi

target_path=$(echo "$input" | "$JQ" -r '.tool_input.file_path // .tool_input.path // ""')

# Auto-allow Write/Edit to the sandbox script directory
if [ "$tool_name" = "Write" ] || [ "$tool_name" = "Edit" ]; then
  # Reject paths with traversal
  if [[ "$target_path" == *..* ]]; then
    exit 0
  fi
  if [[ "$target_path" == */.claude/deno-sandbox/* ]]; then
    "$JQ" -n '{
      hookSpecificOutput: {
        hookEventName: "PermissionRequest",
        decision: { behavior: "allow" }
      }
    }'
    exit 0
  fi
  exit 0
fi

# Only handle Read, Grep, Glob from here
case "$tool_name" in
  Read|Grep|Glob) ;;
  *) exit 0 ;;
esac

if [ -z "$target_path" ]; then
  exit 0
fi

# Reject paths with traversal
if [[ "$target_path" == *..* ]]; then
  exit 0
fi

# Load granted read paths from session state
STATE_FILE="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/sessions/${DENO_SANDBOX_SESSION_ID:-unknown}"
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
