# github-action

> **Deprecated — no longer maintained.** It still works, but it is no longer recommended by `/hive:align` and won't get further updates. If you already installed the PR review workflow, re-run `/github-action:setup` to pick up the fix described below.

Set up the Claude Code GitHub Action so `@claude` mentions on issues and PRs trigger autonomous Claude sessions.

## Security note

The PR review workflow runs in the base repository with its secrets, and checks out the pull request's branch so Claude can read the code under review. A fork can change any file outside `.github/workflows/`, so three things in that workflow are load-bearing and should stay if you edit it:

- The initial checkout is pinned to the PR's base commit. On `pull_request_review` the default ref is the merge ref, which already contains the fork's changes — the dependency install step would run the PR author's install hooks.
- `core.hooksPath` is disabled before `gh pr checkout`. Otherwise, in a repo where the install step configures hooks (husky and friends), checking the branch out is by itself enough to run a fork-supplied hook, and Claude's later `git commit`/`git push` would run more.
- The prompt, `update-comment.sh`, `.claude/` and `CLAUDE.md` are restored from the base commit afterwards, and `.mcp.json` is dropped. Otherwise a fork supplies Claude's instructions, its tool permissions, and the script the allowlist lets it run.

Known limits, none of which this deprecated plugin will fix. Reviewing untrusted code is inherently exposed to prompt injection from the diff itself. If your base `.claude/settings.json` defines Claude hooks that run scripts from the repo, or your base `CLAUDE.md` `@import`s files outside `.claude/`, those targets still come from the PR's tree. And because the restore writes to the index, a PR that legitimately edits `CLAUDE.md`, the prompt, or `update-comment.sh` can have that edit reverted if Claude commits. If you want a review bot on a repo that takes fork PRs, use one that keeps the untrusted checkout out of the workspace root entirely.

## Motivation

Claude Code has a built-in `/install-github-app` command that generates template workflow files. This plugin replaces those templates with better defaults:

- **Opus by default** — Uses Claude Opus instead of Sonnet for higher-quality autonomous work.
- **Minimal, flexible prompts** — Separate prompt templates for issues and PR reviews that you can edit to control how Claude behaves.
- **Tracking comments with mid-session feedback** — Claude posts a progress comment and checks for new human comments each time it updates, so you can steer it while it works.
- **Plugin support in CI** — Detects your installed plugins and configures them in the GitHub Action, including marketplace registration and secret management.
- **Permission check** — Verifies that your project has bash permissions configured so Claude can run build/test commands autonomously in CI.
- **Ecosystem-aware caching** — Detects your project type (Python/uv, Rust, Node/bun, etc.) and adds the right dependency caching steps.

## How It Works

**Issue workflow** — When an issue is opened with `@claude` in the title or body, or when `@claude` is mentioned in a comment, Claude reads the issue, implements changes on a branch, creates a PR, and posts a tracking comment with progress updates.

**PR review workflow** — When a non-approving review is submitted on a Claude-authored PR (or `@claude` is mentioned in a PR comment), Claude addresses the feedback, pushes fixes, and updates the tracking comment.
