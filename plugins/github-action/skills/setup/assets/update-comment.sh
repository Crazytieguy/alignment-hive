#!/bin/bash
# Wrapper around `gh issue comment` that:
# 1. Prepends a link to the current GitHub Actions run
# 2. Posts a new comment on first call, edits it on subsequent calls
# 3. Checks for new human comments and prints them to stdout
#
# Usage: bash .github/scripts/update-comment.sh "<body>"
#
# Required env vars:
#   ISSUE_NUMBER       - issue or PR number
#   GITHUB_REPOSITORY  - owner/repo (set automatically by GitHub Actions)
#   GITHUB_SERVER_URL  - e.g. https://github.com (set automatically)
#   GITHUB_RUN_ID      - current workflow run ID (set automatically)

set -e

BODY="$1"
POSTED_FLAG="/tmp/claude-comment-posted"
STATE_FILE="/tmp/claude-last-comment-id"

# Prepend run link
RUN_URL="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
FULL_BODY="[View run]($RUN_URL)

$BODY"

# Post or edit
if [ -f "$POSTED_FLAG" ]; then
  gh issue comment "$ISSUE_NUMBER" --repo "$GITHUB_REPOSITORY" --edit-last --body "$FULL_BODY"
else
  gh issue comment "$ISSUE_NUMBER" --repo "$GITHUB_REPOSITORY" --body "$FULL_BODY"
  touch "$POSTED_FLAG"
fi

# Check for new human comments
comments=$(gh api "repos/$GITHUB_REPOSITORY/issues/$ISSUE_NUMBER/comments" 2>/dev/null) || exit 0

last_seen=$(cat "$STATE_FILE" 2>/dev/null || echo "0")
last_seen=${last_seen:-0}

# Update state with latest comment ID
echo "$comments" | jq -r 'if length > 0 then last.id else 0 end' > "$STATE_FILE"

# Filter new comments from humans only
new_comments=$(echo "$comments" | jq -r --argjson last_seen "$last_seen" \
  '[.[] | select(.id > $last_seen and .user.login != "claude[bot]")]
   | map("@" + .user.login + ": " + .body)
   | join("\n\n")')

if [ -n "$new_comments" ]; then
  echo "New comments posted on the issue while you're working:"
  echo ""
  echo "$new_comments"
fi
