# Design Decisions

## Agent Mode Over Tag Mode

The action has two modes. Tag mode (no `prompt` input) uses an ~870-line built-in prompt that can't be replaced, only appended to. Agent mode (with `prompt`) gives full control. Agent mode requires explicitly configuring allowed tools and permission mode, but the tradeoff is worth it for control over Claude's behavior.

## Wrapper Script for Tracking Comments

The `update_claude_comment` MCP tool (built into the action) does not work in agent mode because it requires a `CLAUDE_COMMENT_ID` that only tag mode creates. The wrapper script (`update-comment.sh`) replaces it with `gh issue comment --edit-last` and adds mid-session notification checking.

## Mid-Session Notifications

With `cancel-in-progress: true`, a new comment would cancel the running job and lose its progress. Instead `cancel-in-progress: false` lets it finish, the wrapper script prints new human comments on every tracking update, and a skip-check step keeps the queued run from duplicating work: it compares the timestamp of `claude[bot]`'s most recently updated comment against the triggering event's timestamp and skips if Claude already commented after the trigger. Checking who left the last comment would not work, because PR reviews (e.g. "request changes") do not appear in the issue comments endpoint.

## Separate Issue and PR Workflows

Different triggers, branch handling, and prompt context make two focused files cleaner than one file with conditionals. The issue workflow creates a new branch; the PR workflow checks out the existing PR branch.

## Permission Model

- `--permission-mode acceptEdits` auto-approves Edit and Write
- Every Bash command needs an allow rule: the workflow's `--allowedTools` covers the gh/git commands the prompts name; build and test commands come from the repo's `.claude/settings.json`, which the action reads
- `Bash(command:*)` colon syntax (not space) handles multiline command arguments
- `Bash(git push origin HEAD)` exact match prevents pushing to arbitrary branches
- `--disallowedTools TodoWrite` forces Claude to use the visible tracking comment instead of an internal todo list
- Branch protection rules on `main` are the strongest safeguard; permission patterns are a soft guardrail

## PR Auto-Trigger

Any non-approval review on a PR authored by `claude[bot]` triggers Claude automatically. Reviewers don't need to remember `@claude`. Approvals are filtered out (`review.state != 'approved'`) to avoid wasting runs.

## Prompts as Files

Prompts live in `.github/prompts/` rather than inline in the workflow YAML. This keeps workflow files clean and makes prompt iteration easier (readable diffs). The workflow loads them via `sed` with `{{NUMBER}}`/`{{REPOSITORY}}` placeholder substitution.

## Plugin Support

The action's `plugin_marketplaces` and `plugins` inputs install plugins before Claude runs, giving CI sessions access to the same MCP servers, skills, hooks, and subagents available locally. Plugin tool permissions come from `.claude/settings.json` (which the action reads from the repo), so no `--allowedTools` changes are needed. Plugins that need secrets use GitHub repo secrets passed as env vars on the action step.

## OAuth Token Model

When using `claude_code_oauth_token`, the action authenticates via the Claude GitHub App which creates its own installation token with `contents: write`, `pull-requests: write`, `issues: write`. The `permissions:` block in the workflow YAML governs the default `GITHUB_TOKEN` which Claude doesn't primarily use. The App token is what matters.
