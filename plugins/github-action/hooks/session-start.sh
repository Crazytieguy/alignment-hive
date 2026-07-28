#!/bin/bash
# SessionStart hook for github-action.
# Silent unless this repo has a PR review workflow installed from before the
# security fix. `core.hooksPath` is the marker: the hardened template disables
# git hooks before checking out the PR branch, so its presence means the
# workflow has been regenerated. Never fails the session.
#
# Messages use ONLY textual backslash-u001b escapes (JSON-decoded by Claude Code);
# this file must contain no literal control bytes.

WORKFLOW="${CLAUDE_PROJECT_DIR:-.}/.github/workflows/claude-pr.yml"

[ -f "$WORKFLOW" ] || exit 0
grep -q 'core\.hooksPath' "$WORKFLOW" 2>/dev/null && exit 0

U='\'"u001b"
BOLD="${U}[1m"
MAGENTA="${U}[1;35m"
RESET="${U}[0m"

printf '{"systemMessage": "%sgithub-action:%s this repo'"'"'s PR review workflow predates a security fix - regenerate it with %s/github-action:setup%s"}\n' \
  "$BOLD" "$RESET" "$MAGENTA" "$RESET"
exit 0
