# alignment-hive

Shared tooling and knowledge layer for the AI alignment community.

## About This Repo

@README.md explains what this is. Keep it up to date as the project evolves.

This is a **bun + cargo monorepo**:
- `packages/web/` - TanStack Start web app (alignment-hive.com)
- `packages/hive-cli/` - CLI for session extraction and sharing
- `packages/session-data/` - Shared code (schemas, parsing)
- `crates/` - Rust crates (remote-kernels MCP server)
- `plugins/` - Plugin distributions

## Working on the Code

**For web app**: Read [packages/web/README.md](packages/web/README.md) for local development setup

**For CLI**: Read [packages/hive-cli/CLAUDE.md](packages/hive-cli/CLAUDE.md) for development guidelines

**For Rust crates**: Read [crates/CLAUDE.md](crates/CLAUDE.md) for quality gates and conventions

## Running Scripts

Run workspace scripts from the repo root using `bun run --filter`:

```bash
# All workspaces
bun run --filter '*' lint
bun run --filter '*' build
bun run --filter '*' format

# Specific workspace
bun run --filter '@alignment-hive/hive-cli' test
bun run --filter '@alignment-hive/hive-cli' lint
bun run --filter '@alignment-hive/web' lint
```

Workspaces without the script are skipped (no error).

For workspace-specific tasks like dev servers:
```bash
bun run --filter '@alignment-hive/web' dev
```

## Adding New Plugins

New plugins must be registered in `.claude-plugin/marketplace.json` to appear in the marketplace. Add an entry with `name`, `source`, and `description`. The `name` must match the plugin's folder name under `plugins/` (e.g., `"name": "autopilot"` for `plugins/autopilot/`).

## Plugin Versioning

When updating plugin content (skills, commands, hooks, etc.), you must bump the version in the plugin's `plugin.json` for users to receive the update. The auto-update system compares installed versions with marketplace versions — without a version bump, changes won't propagate to users.

Plugin locations:
- `plugins/autopilot/.claude-plugin/plugin.json`
- `plugins/github-action/.claude-plugin/plugin.json`
- `plugins/hive/.claude-plugin/plugin.json`
- `plugins/mats/.claude-plugin/plugin.json`
- `plugins/llms-fetch-mcp/.claude-plugin/plugin.json`
- `plugins/remote-kernels/.claude-plugin/plugin.json`

The hive plugin has a `cli-version` file (`plugins/hive/cli-version`) that must match the version in `packages/hive-cli/package.json`. This controls which binary version users download. Always bump both together.

**Auto-expanding bash commands fail hard.** If `` !`command` `` returns non-zero, the entire skill/agent/command fails to load. Use fallbacks like `command 2>/dev/null || echo "fallback"`.

## Python

Use [uv](https://docs.astral.sh/uv/) with inline dependencies (PEP 723). Run scripts with `uv run script.py`.

## Ad-hoc Scripts

Only `/tmp/claude-execution-allowed/alignment-hive/` is approved for ad-hoc scripts. JavaScript/TypeScript scripts run with `bun /tmp/claude-execution-allowed/alignment-hive/<script-name>`. Bash scripts run with `bash /tmp/claude-execution-allowed/alignment-hive/<script-name>`.

## Codebase Exploration

Always use `precis` for codebase exploration. Run `precis .` for a full overview, or `precis src/some/directory` to zoom into a specific area.
