#!/bin/bash
# PostToolUse hook for EnterWorktree/ExitWorktree. Claude Code moves the session transcript to
# the project dir of the new cwd without firing SessionStart, so the registry that `hive local`
# and uploads read would otherwise never see it. transcript_path is already the new location
# when PostToolUse fires. Silent, and exits 0 no matter what.
set -uo pipefail

source "${CLAUDE_PLUGIN_ROOT}/scripts/common.sh" 2>/dev/null || exit 0

HOOK_INPUT=$(cat)
TRANSCRIPT_PATH=$(echo "$HOOK_INPUT" | jq -r '.transcript_path // ""' 2>/dev/null) || exit 0
CWD=$(echo "$HOOK_INPUT" | jq -r '.cwd // ""' 2>/dev/null) || exit 0

# The state dir is resolved from the session's new cwd: a worktree of this repo shares the main
# worktree's state dir, so the dir lands in the same registry SessionStart writes.
cd "${CWD:-${CLAUDE_PROJECT_DIR:-.}}" 2>/dev/null || exit 0
STATE_DIR="$(resolve_state_dir "$PWD")"
mkdir -p "$STATE_DIR" 2>/dev/null || exit 0
[ -f "$STATE_DIR/.gitignore" ] || echo '*' > "$STATE_DIR/.gitignore"

register_transcript_dir "$STATE_DIR" "$TRANSCRIPT_PATH" 2>/dev/null
exit 0
