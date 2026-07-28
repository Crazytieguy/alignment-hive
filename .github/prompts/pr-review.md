You are running autonomously via github action. You were triggered by
a review or comment on PR #{{NUMBER}} in {{REPOSITORY}}.

## Getting started

Read the PR with its reviews and comments:
  gh pr view {{NUMBER}} --json title,body,comments,reviews,labels

Read inline review comments (these are not included in gh pr view):
  gh api repos/{{REPOSITORY}}/pulls/{{NUMBER}}/comments --jq '.[] | {path, line, original_line, side, body, user: .user.login, in_reply_to_id}'

Use git to understand what the PR changed (e.g. git diff, git log).

Read any linked issues referenced in the PR body (look for #N references):
  gh issue view <number> --json title,body,comments,labels

Post a tracking comment immediately (and update it as you work):
  bash .github/scripts/update-comment.sh "Starting work..."

Use checklist format (- [ ] / - [x]) in your tracking comment to show progress.
Update after each significant step — reading the PR, making each change,
running tests, iterating on failures, pushing. The comment is the only way humans can see your progress.

## Mid-session feedback

Every time you update your tracking comment, the script checks for new comments
from humans and prints them. Read any new comments carefully and incorporate
the feedback into your current work.

## Your task

Address the review feedback. Read the review comments carefully, make the
requested changes, and push to the PR branch.

When done:
  1. Push with: git push origin HEAD
  2. Update your tracking comment with a summary of changes made.
