---
name: github-action
description: This skill should be used when the user asks to "set up the github action", "configure async claude", "enable @claude on issues", "set up claude code action", "github action integration", "autonomous claude on github", or wants Claude to work on issues and PRs autonomously via GitHub Actions.
---

# GitHub Action Setup

Set up the Claude Code GitHub Action so that `@claude` mentions on issues and PRs trigger autonomous Claude sessions. Claude reads the issue, implements changes on a branch, creates a PR, and responds to PR reviews — all without human intervention.

## Prerequisites

Before starting, verify:

1. **Git repo with GitHub remote.** Run `git remote -v` to confirm a GitHub remote exists.
2. **No existing setup.** Check if `.github/workflows/claude-issue.yml` already exists. If it does, ask the user whether to overwrite or skip.

## Step 1: OAuth Authentication

The user must run a built-in Claude Code command to authenticate. Tell them:

> Run `/install-github-app` in your Claude Code CLI. Here's what to expect:
>
> 1. **Install the GitHub App** — a browser window will open to install the Claude GitHub App on your repo. Follow the prompts.
> 2. **Select workflows** — it will ask which workflows to install. Select any option (it doesn't matter which — we'll replace them with our own). You must select at least one to proceed.
> 3. **Authentication** — choose "Use my Claude subscription" (unless you have a specific API key you'd rather use).
> 4. **Browser tab** — it will open a browser tab to create a PR with template workflow files. **Just close that tab** — we'll generate our own opinionated workflows instead.
>
> Let me know when you're done!

Wait for user confirmation before proceeding.

## Step 2: Detect Ecosystem & Write Files

### Detect ecosystem

Scan the repo root for ecosystem indicators to determine the right dependency caching step:

| Indicator | Ecosystem |
|---|---|
| `uv.lock` or `pyproject.toml` (no other lockfile) | Python + uv |
| `requirements.txt` (no `uv.lock`) | Python + pip |
| `Cargo.toml` | Rust |
| `package.json` + `package-lock.json` | Node.js + npm |
| `package.json` + `bun.lock` or `bun.lockb` | Node.js + bun |

Read the matching snippet from `references/cache-steps.md`. If no match or multiple ecosystems, leave the `# CACHE_STEP` comment as-is and tell the user to add caching later.

### Write files

Create the destination directories (`.github/workflows`, `.github/scripts`, `.github/prompts`) then copy asset files directly using `cp`. For the two workflow files, after copying, replace the `# CACHE_STEP` and `# INSTALL_STEP` comments with the detected snippets from the reference file. The other three files are copied as-is.

| Asset | Destination |
|---|---|
| `assets/claude-issue.yml` | `.github/workflows/claude-issue.yml` |
| `assets/claude-pr.yml` | `.github/workflows/claude-pr.yml` |
| `assets/update-comment.sh` | `.github/scripts/update-comment.sh` |
| `assets/issue-prompt.md` | `.github/prompts/issue.md` |
| `assets/pr-review-prompt.md` | `.github/prompts/pr-review.md` |

## Step 3: Check Permissions

Read `.claude/settings.json` and `.claude/settings.local.json`. Claude in the GitHub Action uses these for bash permissions. Check if permissions are properly configured using the same heuristic as best-practices:
- At least 15 allow rules total
- At least 3 deny rules
- Has project-specific commands

Note the result for the summary — do not message the user yet.

## Step 4: Summary

List all files created and summarize:

- `.github/workflows/claude-issue.yml` — triggers on `@claude` in issues
- `.github/workflows/claude-pr.yml` — triggers on PR reviews and `@claude` in PR comments
- `.github/scripts/update-comment.sh` — tracking comment wrapper with mid-session feedback
- `.github/prompts/issue.md` — issue prompt template
- `.github/prompts/pr-review.md` — PR review prompt template

**If permissions are unconfigured** (from Step 3), include a warning:
> Claude in the GitHub Action uses your project's `.claude/settings.json` for bash permissions. Without proper permissions, Claude won't be able to run build/test commands autonomously.

Offer to invoke `/mats:permissions` to configure them.

Remind the user to:
1. **Clean up** — `/install-github-app` created a branch like `add-claude-github-actions-*`. They can delete it from the GitHub UI or ignore it.
2. **Customize prompts** (optional) — `.github/prompts/issue.md` and `.github/prompts/pr-review.md` control how Claude behaves autonomously. They're short and readable.
3. **Commit and push** these files to the repo
4. **Enable branch protection** on `main` as defense in depth (Claude pushes to `claude/issue-N` branches, not `main`). Offer to help configure this — recommended rules: require pull request before merging (with at least 1 review), restrict direct pushes, restrict deletions, and block force pushes.
5. **Test it** by creating an issue mentioning `@claude` with a simple task

## How to Use the Integration

After setup, explain how the user interacts with Claude on GitHub:

### Triggering Claude

- **Issues:** Mention `@claude` in an issue title, body, or comment. Claude creates a branch, implements the changes, and opens a PR.
- **PRs:** Submit a review (requesting changes or commenting) on a Claude-authored PR, and Claude responds automatically. On any PR, mention `@claude` in a comment to trigger it.

### While Claude is Working

- Claude posts a **tracking comment** on the issue or PR with a checklist showing progress. Click the "View run" link to see the full GitHub Actions log.
- **Post follow-up comments** at any time — Claude checks for new comments each time it updates its tracking comment and incorporates your feedback mid-session.
- Claude typically works for 5–15 minutes depending on task complexity.

### Reviewing Claude's Work

- Claude opens a PR referencing the original issue. Review it like any other PR.
- **Request changes** via a GitHub review and Claude will address them automatically (no need to `@claude` on its own PRs).
- Leave **inline review comments** on specific lines for targeted feedback.
- Once satisfied, approve and merge the PR as usual.

## Additional Resources

### Reference Files

- **`references/design-decisions.md`** — Explains why specific patterns were chosen (agent mode, wrapper script, permission model, etc.). Consult when the user asks "why" questions.
- **`references/cache-steps.md`** — YAML snippets for dependency caching per ecosystem.
