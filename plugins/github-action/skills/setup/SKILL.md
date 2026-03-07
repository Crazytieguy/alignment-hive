---
name: setup
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

## Step 3: Configure Plugins

The GitHub Action supports installing plugins from marketplaces. Detect the user's installed plugins and offer to include them in CI.

### Detect installed plugins

Read `.claude/settings.json` and `.claude/settings.local.json` looking for:
- `enabledPlugins` — all enabled plugins (from any marketplace)
- `pluginMarketplaces` — all registered marketplaces

### Present findings

Show the user which plugins and marketplaces were detected. Use AskUserQuestion to confirm which plugins to include in CI.

If the user selects no plugins, skip the rest of this step.

### Update workflow files

Replace the `# PLUGINS` comment in both workflow files with `plugin_marketplaces` and `plugins` inputs. Only include marketplaces that have at least one selected plugin. Marketplace references in settings may be shorthand (e.g. `Crazytieguy/alignment-hive`) — convert to full git URLs for the action. Example:

```yaml
          plugin_marketplaces: |
            https://github.com/owner/repo.git
          plugins: |
            some-plugin@some-marketplace
```

### Handle secrets

For plugins that need secrets (e.g. `remote-kernels` needs `RUNPOD_API_KEY`), replace the `# PLUGIN_ENV` comment in both workflow files with the required env var lines:

```yaml
          RUNPOD_API_KEY: ${{ secrets.RUNPOD_API_KEY }}
```

For unknown plugins, ask the user if any environment variables are needed.

Then offer to set each secret via `gh secret set`, piping the value from the local environment (`.env`, `.env.local`, or shell). If that's not possible, explain how to set it manually in the repo's Settings → Secrets.

If no plugins need secrets, remove the `# PLUGIN_ENV` comment line from both workflow files.

## Step 4: Check Permissions

Read `.claude/settings.json` and `.claude/settings.local.json`. Claude in the GitHub Action uses these for bash permissions. Check if permissions are properly configured:
- At least 15 allow rules total
- At least 3 deny rules
- Has project-specific commands

Note the result for the summary — do not message the user yet.

## Step 5: Summary

List all files created and summarize:

- `.github/workflows/claude-issue.yml` — triggers on `@claude` in issues
- `.github/workflows/claude-pr.yml` — triggers on PR reviews and `@claude` in PR comments
- `.github/scripts/update-comment.sh` — tracking comment wrapper with mid-session feedback
- `.github/prompts/issue.md` — issue prompt template
- `.github/prompts/pr-review.md` — PR review prompt template

**If plugins were configured** (from Step 3), list which plugins will be available in CI. Note which secrets were set and remind about any that still need to be added manually.

**If permissions are unconfigured** (from Step 4), include a warning:
> Claude in the GitHub Action uses your project's `.claude/settings.json` for bash permissions. Without proper permissions, Claude won't be able to run build/test commands autonomously.

Recommend installing the autopilot plugin (`autopilot@alignment-hive`) if not already installed — it includes a permissions setup flow.

Tell the user the following:

**Next steps:**
1. **Clean up** — `/install-github-app` created a branch like `add-claude-github-actions-*`. Delete it from the GitHub UI or ignore it.
2. **Customize prompts** (optional) — `.github/prompts/issue.md` and `.github/prompts/pr-review.md` control how Claude behaves autonomously. They're short and readable.
3. **Commit and push** these files to the repo.
4. **Enable branch protection** on `main` as defense in depth (Claude pushes to `claude/issue-N` branches, not `main`). Offer to help configure this — recommended rules: require pull request before merging (with at least 1 review), restrict direct pushes, restrict deletions, and block force pushes.
5. **Test it** by creating an issue mentioning `@claude` with a simple task.

**Using the integration:**
- Mention `@claude` in an issue or PR comment to trigger a session. On Claude-authored PRs, submitting a review triggers it automatically.
- Claude posts a **tracking comment** with progress. You can post follow-up comments at any time — Claude checks for new feedback each time it updates.
- Review Claude's PRs normally. Request changes via a review and Claude addresses them automatically.

## Additional Resources

### Reference Files

- **`references/design-decisions.md`** — Explains why specific patterns were chosen (agent mode, wrapper script, permission model, etc.). Consult when the user asks "why" questions.
- **`references/cache-steps.md`** — YAML snippets for dependency caching per ecosystem.
