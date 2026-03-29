#!/bin/bash
set -euo pipefail

# Auto-allow:
# 1. deno-sandbox-grant help/no-args (read-only)
# 2. deno-sandbox with a registered script path (session or agent)
# 3. Read/Write/Edit to registered sandbox script files
# 4. Read/Grep/Glob on directories granted via deno-sandbox-grant

input=$(cat)

# shellcheck source=../scripts/find-jq.sh
source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || exit 0

eval "$( echo "$input" | "$JQ" -r '
  "tool_name=" + (.tool_name // "" | @sh),
  "target_path=" + (.tool_input.file_path // .tool_input.path // "" | @sh),
  "session_id=" + (.session_id // "" | @sh),
  "bash_command=" + (.tool_input.command // "" | @sh)
')"

sandbox_dir="${CLAUDE_PROJECT_DIR}/.claude/deno-sandbox"

# --- Helpers ---

emit_allow() {
  "$JQ" -n '{ hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } } }'
}

emit_deny() {
  "$JQ" -n --arg msg "$1" '{ hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "deny", message: $msg } } }'
}

# Load allowed script basenames (session ID + registered agent IDs).
# Called lazily — only when we actually need to check sandbox scripts.
_basenames_loaded=false
allowed_basenames=()
load_allowed_basenames() {
  if [ "$_basenames_loaded" = true ]; then return; fi
  _basenames_loaded=true
  if [ -n "$session_id" ]; then
    allowed_basenames+=("${session_id}.ts")
    local registry="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}/sessions/${session_id}.agents"
    if [ -f "$registry" ]; then
      while IFS= read -r agent_id; do
        [ -n "$agent_id" ] && allowed_basenames+=("${agent_id}.ts")
      done < "$registry"
    fi
  fi
}

is_allowed_script() {
  load_allowed_basenames
  local path="$1"
  for allowed in "${allowed_basenames[@]}"; do
    if [ "$path" = "$sandbox_dir/$allowed" ]; then
      return 0
    fi
  done
  return 1
}

# --- Bash tool ---

if [ "$tool_name" = "Bash" ]; then
  # Auto-allow deno-sandbox and deno-sandbox-grant with no args or --help (read-only)
  case "$bash_command" in
    deno-sandbox-grant|"deno-sandbox-grant --help"|"deno-sandbox-grant -h"|\
    "deno-sandbox --help"|"deno-sandbox -h")
      emit_allow
      exit 0 ;;
  esac

  # Auto-allow deno-sandbox with a registered script path.
  # Matches: "deno-sandbox <path>" and "<stuff> | deno-sandbox <path>"
  # Word boundary: require start-of-string or pipe/whitespace before "deno-sandbox"
  if [[ "$bash_command" =~ (^|[|[:space:]])deno-sandbox[[:space:]]+([^|]+)$ ]]; then
    script_arg="${BASH_REMATCH[2]}"
    script_arg="${script_arg#"${script_arg%%[![:space:]]*}"}"
    script_arg="${script_arg%"${script_arg##*[![:space:]]}"}"

    # Check basename against allowed list. The deno-sandbox script itself
    # does full path validation (realpath + sandbox dir check) at runtime.
    script_basename=$(basename "$script_arg")
    if is_allowed_script "$sandbox_dir/$script_basename"; then
      emit_allow
    else
      emit_deny "Script path is not registered for this session. Use your assigned sandbox script file."
    fi
    exit 0
  fi

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

# Auto-allow Read/Write/Edit to registered sandbox script files
if is_allowed_script "$target_path"; then
  emit_allow
  exit 0
fi

# Deny writes to unregistered .ts files in the sandbox dir
if [ "$tool_name" = "Write" ] || [ "$tool_name" = "Edit" ]; then
  if [[ "$target_path" == "$sandbox_dir"/* ]] || [[ "$target_path" == */.claude/deno-sandbox/* ]]; then
    # Only block writes to script files (.ts), not config/declaration files (.d.ts, .json, etc.)
    case "$target_path" in
      *.d.ts) ;;
      *.ts) emit_deny "Write to your assigned sandbox script file instead." ;;
    esac
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
