#!/bin/bash
set -euo pipefail

LOG_FILE="$HOME/.cache/autopilot/bootstrap.log"
mkdir -p "$(dirname "$LOG_FILE")" 2>/dev/null

# Capture all stderr to the log file
exec 2>>"$LOG_FILE"

STATE_FILE="$CLAUDE_PROJECT_DIR/.claude/autopilot/state.json"

# ANSI escape codes — use printf to get real escape chars (for jq --arg)
B=$(printf '\033[1;32m') # bold green
R=$(printf '\033[0m')    # reset

# On any error, log and inform user
trap 'echo "$0: line $LINENO: unexpected error" >&2; echo "{\"systemMessage\":\"\u001b[1;32mautopilot:\u001b[0m session start error, see $LOG_FILE\"}"' ERR

# Find jq — check only, no download yet
if command -v jq >/dev/null 2>&1; then
  JQ="jq"
elif [ -x "$HOME/.cache/autopilot/jq" ]; then
  JQ="$HOME/.cache/autopilot/jq"
else
  # No jq yet — async hook will bootstrap it and report back
  echo "{\"systemMessage\": \"\u001b[1;32mautopilot:\u001b[0m bootstrapping jq, auto-deny will activate once ready\"}"
  exit 0
fi

# --- Parse hook input ---
HOOK_INPUT=$(cat)
SESSION_ID=$(echo "$HOOK_INPUT" | "$JQ" -r '.session_id // ""')

# --- Deno sandbox environment setup ---
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  echo "export DENO_SANDBOX_SESSION_ID=\"$SESSION_ID\"" >> "$CLAUDE_ENV_FILE"
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

# No state file → not configured
if [ ! -f "$STATE_FILE" ]; then
  echo "{\"systemMessage\": \"\u001b[1;32mautopilot:\u001b[0m not configured, run \u001b[1;35m/autopilot:setup\u001b[0m\"}"
  exit 0
fi

# --- Build output ---
STATUS_MSG=""
ADDITIONAL_CONTEXT=""

# Check autonomous mode
if "$JQ" -e '.autonomous_mode == true' "$STATE_FILE" >/dev/null 2>&1; then
  STATUS_MSG="${B}autopilot:${R} autonomous mode active"
else
  STATUS_MSG="${B}autopilot:${R} installed"
fi

# Deno sandbox additionalContext
if command -v deno >/dev/null 2>&1 || [ -x "$HOME/.deno/bin/deno" ]; then
  SANDBOX_SCRIPT=".claude/deno-sandbox/$SESSION_ID.ts"
  read -r -d '' ADDITIONAL_CONTEXT << CONTEXT || true
## deno-sandbox

\`deno-sandbox\` runs JavaScript/TypeScript in a secure Deno sandbox. Default: read-only access to current directory. Network, writes, and env access are all blocked unless explicitly granted.

**Usage:**
\`\`\`
# 1. Write code to $SANDBOX_SCRIPT
# 2. Run the sandbox:
deno-sandbox
# With data piped in:
cat data.csv | deno-sandbox
\`\`\`

\`npm:\` and \`jsr:\` imports work out of the box (packages are fetched from their registries; sandbox permissions still apply to what the code can do): \`import { parse } from "npm:csv-parse/sync";\`

**Granting permissions** (each grant requires user approval):
\`\`\`
deno-sandbox-grant --allow-write=. --allow-net=api.example.com
\`\`\`
Available: \`--allow-{read,write,net,env,import}=<scope>\`. Follow the principle of least privilege — request only the most specific permissions needed. Read and write are separate permissions — granting write to a path does not grant read.

Request any necessary permissions as early as possible. The user is typically available to review grants at the start of a session and will want you to work without interruption afterward.
CONTEXT
fi

# Emit output
if [ -n "$ADDITIONAL_CONTEXT" ]; then
  "$JQ" -n --arg msg "$STATUS_MSG" --arg ctx "$ADDITIONAL_CONTEXT" '{
    systemMessage: $msg,
    hookSpecificOutput: {
      hookEventName: "SessionStart",
      additionalContext: $ctx
    }
  }'
else
  "$JQ" -n --arg msg "$STATUS_MSG" '{ systemMessage: $msg }'
fi
