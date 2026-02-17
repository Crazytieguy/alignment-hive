# Design Decisions

Reference for explaining why the GitHub Action setup uses specific patterns. Read this when the user asks "why" questions about the configuration.

## Agent Mode Over Tag Mode

The action has two modes. Tag mode (no `prompt` input) uses an ~870-line built-in prompt that can't be replaced, only appended to. Agent mode (with `prompt`) gives full control. Agent mode requires explicitly configuring allowed tools and permission mode, but the tradeoff is worth it for control over Claude's behavior.

## Wrapper Script for Tracking Comments

The `update_claude_comment` MCP tool (built into the action) does not work in agent mode because it requires a `CLAUDE_COMMENT_ID` that only tag mode creates. The wrapper script (`update-comment.sh`) replaces it with `gh issue comment --edit-last` and adds mid-session notification checking.

## Mid-Session Notifications

With `cancel-in-progress: true`, new comments cancel running jobs and Claude loses all progress. Instead:
- `cancel-in-progress: false` lets the running job continue
- The wrapper script checks for new human comments on every tracking update
- A skip-check step prevents queued runs from duplicating work

## Skip Check Step

Queued runs check if the most recently *updated* comment is by `claude[bot]`. If so, Claude already picked up the triggering comment via the wrapper script. Uses `sort_by(.updated_at)` because sorting by creation time would miss tracking comment edits.

## Separate Issue and PR Workflows

Different triggers, branch handling, and prompt context make two focused files cleaner than one file with conditionals. The issue workflow creates a new branch; the PR workflow checks out the existing PR branch.

## Permission Model

- `--permission-mode acceptEdits` grants file operation tools (Edit, Write, Read)
- Bash is disabled by default; only specific commands are whitelisted via `--allowedTools`
- `Bash(command:*)` colon syntax (not space) handles multiline command arguments
- `Bash(git push origin HEAD)` exact match prevents pushing to arbitrary branches
- `--disallowedTools TodoWrite` forces Claude to use the visible tracking comment instead of an internal todo list
- Branch protection rules on `main` are the strongest safeguard; permission patterns are a soft guardrail

## PR Auto-Trigger

Any non-approval review on a PR authored by `claude[bot]` triggers Claude automatically. Reviewers don't need to remember `@claude`. Approvals are filtered out (`review.state != 'approved'`) to avoid wasting runs.

## Prompts as Files

Prompts live in `.github/prompts/` rather than inline in the workflow YAML. This keeps workflow files clean and makes prompt iteration easier (readable diffs). The workflow loads them via `sed` with `{{NUMBER}}`/`{{REPOSITORY}}` placeholder substitution.

## OAuth Token Model

When using `claude_code_oauth_token`, the action authenticates via the Claude GitHub App which creates its own installation token with `contents: write`, `pull-requests: write`, `issues: write`. The `permissions:` block in the workflow YAML governs the default `GITHUB_TOKEN` which Claude doesn't primarily use. The App token is what matters.
