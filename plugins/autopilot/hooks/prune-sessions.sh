#!/bin/bash
# Async SessionStart hook: deletes per-session state untouched for 30 days. Nothing else can: SessionEnd
# doesn't fire for long-lived background sessions and nothing fires when a session is deleted. That
# state is the grants files and agent registries (sessions/<id>, sessions/<id>.agents) in the data
# dir, and the <session id>.ts / <agent id>.ts scripts in .claude/deno-sandbox/. The current
# session's files are touched first, so a resumed session never loses its grants to the prune.
set -euo pipefail

# shellcheck source=../scripts/find-jq.sh
source "$(dirname "${BASH_SOURCE[0]}")/../scripts/find-jq.sh" || exit 0

session_id=$("$JQ" -r '.session_id // ""' 2>/dev/null) || exit 0
# Without a valid id the touch below can't protect the current session, so prune nothing.
[[ "$session_id" =~ ^[A-Za-z0-9-]+$ ]] || exit 0

# Resolved the same way as in sandbox-permissions.sh.
sessions_dir="${AUTOPILOT_DATA_DIR:-${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}}/sessions"
sandbox_dir="${CLAUDE_PROJECT_DIR:-}/.claude/deno-sandbox"

touch -c "$sessions_dir/$session_id" "$sessions_dir/$session_id.agents" 2>/dev/null || true
[ -n "${CLAUDE_PROJECT_DIR:-}" ] && touch -c "$sandbox_dir/$session_id.ts" 2>/dev/null || true

if [ -d "$sessions_dir" ]; then
  find "$sessions_dir" -maxdepth 1 -type f -mtime +30 -delete 2>/dev/null || true
fi

# Only id-named scripts (hex and hyphens, like session UUIDs and agent ids): a project may keep
# its own scripts here, and those are not the plugin's to delete. tsconfig.json, *.d.ts and
# .gitignore never match.
if [ -n "${CLAUDE_PROJECT_DIR:-}" ] && [ -d "$sandbox_dir" ]; then
  find "$sandbox_dir" -maxdepth 1 -type f -name '*.ts' ! -name '*.d.ts' -mtime +30 2>/dev/null |
    while IFS= read -r script; do
      [[ "$(basename "$script" .ts)" =~ ^[0-9a-f-]{16,36}$ ]] && rm -f "$script"
    done || true
fi

exit 0
