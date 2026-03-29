#!/bin/bash
set -euo pipefail

LOG_FILE="$HOME/.cache/autopilot/error.log"
mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null

# Capture all stderr to the log file
exec 2>>"$LOG_FILE"

STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"

# ANSI escape codes for jq --arg
B=$'\033[1;32m'  # bold green
M=$'\033[1;35m'  # bold magenta
R=$'\033[0m'     # reset

# On any error, log and inform user
trap 'echo "$0: line $LINENO: unexpected error" >&2; echo "{\"systemMessage\":\"\u001b[1;32mautopilot:\u001b[0m error in session-start hook, see $LOG_FILE\"}"' ERR

# shellcheck source=../scripts/find-jq.sh
source "${CLAUDE_PLUGIN_ROOT}/scripts/find-jq.sh" || {
  echo "{\"systemMessage\": \"\u001b[1;32mautopilot:\u001b[0m bootstrapping jq, auto-deny will activate once ready\"}"
  exit 0
}

# --- Parse hook input ---
hook_input=$(cat)
session_id=$(echo "$hook_input" | "$JQ" -r '.session_id // ""')
hook_source=$(echo "$hook_input" | "$JQ" -r '.source // ""')

# On resume, context window is intact and CLAUDE_ENV_FILE can't be overwritten
if [ "$hook_source" = "resume" ]; then
  exit 0
fi

# --- Read state ---
# Each key is "true", "false", or "absent"
if [ ! -f "$STATE_FILE" ]; then
  autonomous="absent"
  sandbox="absent"
else
  state_pair=$("$JQ" -r '[
    (if has("autonomous_mode") then (.autonomous_mode | tostring) else "absent" end),
    (if has("deno_sandbox") then (.deno_sandbox | tostring) else "absent" end)
  ] | join(" ")' "$STATE_FILE")
  read -r autonomous sandbox <<< "$state_pair"
fi

# --- Build status message (9 states: autonomous × sandbox, each true/false/absent) ---
setup="${M}/autopilot:setup${R}"
if [ "$autonomous" = "absent" ] && [ "$sandbox" = "absent" ]; then
  status_msg="${B}autopilot:${R} run ${setup} to configure"
elif [ "$autonomous" = "absent" ]; then
  status_msg="${B}autopilot:${R} run ${setup} to configure autonomous mode"
elif [ "$sandbox" = "absent" ]; then
  status_msg="${B}autopilot:${R} run ${setup} to configure sandbox"
else
  auto_label="autonomous mode enabled"
  [ "$autonomous" = "false" ] && auto_label="autonomous mode disabled"
  sandbox_label="sandbox enabled"
  [ "$sandbox" = "false" ] && sandbox_label="sandbox disabled"
  status_msg="${B}autopilot:${R} ${auto_label}, ${sandbox_label}"
fi

# --- Deno sandbox setup (only when enabled) ---
additional_context=""
if [ "$sandbox" = "true" ]; then
  # Environment setup
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    echo "export DENO_SANDBOX_SESSION_ID=\"$session_id\"" >> "$CLAUDE_ENV_FILE"
    echo "export DENO_SANDBOX_PROJECT_DIR=\"$CLAUDE_PROJECT_DIR\"" >> "$CLAUDE_ENV_FILE"
    echo "export PATH=\"\$HOME/.deno/bin:${CLAUDE_PLUGIN_ROOT}/scripts:\$PATH\"" >> "$CLAUDE_ENV_FILE"
  fi

  # Ensure sandbox script directory exists.
  # .gitignore is only written on first creation — if the directory already
  # exists (e.g. with committed scripts), we don't overwrite the user's choice.
  SANDBOX_DIR="$CLAUDE_PROJECT_DIR/.claude/deno-sandbox"
  if [ ! -d "$SANDBOX_DIR" ]; then
    mkdir -p "$SANDBOX_DIR"
    echo '*' > "$SANDBOX_DIR/.gitignore"
  fi
  # Suppress TS LSP diagnostics (Deno APIs aren't in the project's TS types)
  if [ ! -f "$SANDBOX_DIR/tsconfig.json" ]; then
    cat > "$SANDBOX_DIR/tsconfig.json" << 'TSCONFIG'
{
  "compilerOptions": {
    "noEmit": true,
    "allowJs": true,
    "module": "esnext",
    "moduleResolution": "bundler",
    "target": "esnext",
    "lib": ["esnext"],
    "strict": true
  }
}
TSCONFIG
  fi
  if [ ! -f "$SANDBOX_DIR/deno.d.ts" ]; then
    _tmp="$SANDBOX_DIR/deno.d.ts.tmp"
    if command -v deno >/dev/null 2>&1; then
      deno types > "$_tmp" 2>/dev/null && mv "$_tmp" "$SANDBOX_DIR/deno.d.ts" || rm -f "$_tmp"
    elif [ -x "$HOME/.deno/bin/deno" ]; then
      "$HOME/.deno/bin/deno" types > "$_tmp" 2>/dev/null && mv "$_tmp" "$SANDBOX_DIR/deno.d.ts" || rm -f "$_tmp"
    fi
  fi
  if [ ! -f "$SANDBOX_DIR/globals.d.ts" ]; then
    cat > "$SANDBOX_DIR/globals.d.ts" << 'DTS'
declare module "npm:*";
declare module "jsr:*";
DTS
  fi

  # Emit additionalContext if deno is available
  if command -v deno >/dev/null 2>&1 || [ -x "$HOME/.deno/bin/deno" ]; then
    sandbox_script="$SANDBOX_DIR/$session_id.ts"
    # shellcheck source=../scripts/sandbox-instructions.sh
    additional_context=$(source "${CLAUDE_PLUGIN_ROOT}/scripts/sandbox-instructions.sh" "$sandbox_script" "$SANDBOX_DIR")
  fi
fi

# Emit output
if [ -n "$additional_context" ]; then
  "$JQ" -n --arg msg "$status_msg" --arg ctx "$additional_context" '{
    systemMessage: $msg,
    hookSpecificOutput: {
      hookEventName: "SessionStart",
      additionalContext: $ctx
    }
  }'
else
  "$JQ" -n --arg msg "$status_msg" '{ systemMessage: $msg }'
fi
