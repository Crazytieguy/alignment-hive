# github-action

> **Deprecated — no longer maintained.** It still works, but it is no longer recommended by `/hive:align` and won't get further updates.

Set up the Claude Code GitHub Action so `@claude` mentions on issues and PRs trigger autonomous Claude sessions.

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
