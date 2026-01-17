# alignment-hive

Claude Code infrastructure for AI safety researchers.

## About This Repo

@README.md explains what this is. Keep it up to date as the project evolves.

**Important:** The installation instructions in `README.md` and `web/src/routes/_authenticated/welcome.tsx` must stay in sync. When updating one, update the other.

This is a **bun monorepo**:
- `web/` - TanStack Start web app (alignment-hive.com)
- `hive-mind/` - CLI for session extraction
- `plugins/` - Plugin distributions

## Working on the Code

**For web app**: Read [web/README.md](web/README.md) for local development setup

**For CLI**: Read [hive-mind/CLAUDE.md](hive-mind/CLAUDE.md) for development guidelines. Run CLI commands from the project root: `bun hive-mind/cli/cli.ts <command>`

## Running Scripts

Run workspace scripts from the repo root using `bun run --filter`:

```bash
# All workspaces
bun run --filter '*' lint
bun run --filter '*' build
bun run --filter '*' format

# Specific workspace
bun run --filter '@alignment-hive/hive-mind' test
bun run --filter '@alignment-hive/hive-mind' lint
bun run --filter '@alignment-hive/web' lint
```

Workspaces without the script are skipped (no error).

For workspace-specific tasks like dev servers:
```bash
cd web && bun run dev
```

## Plugin Versioning

When updating plugin content (skills, commands, hooks, etc.), you must bump the version in the plugin's `plugin.json` for users to receive the update. The auto-update system compares installed versions with marketplace versions - without a version bump, changes won't propagate to users.

Plugin locations:
- `plugins/hive-mind/.claude-plugin/plugin.json`
- `plugins/mats/.claude-plugin/plugin.json`
- `plugins/llms-fetch-mcp/.claude-plugin/plugin.json`

**Auto-expanding bash commands fail hard.** If `!`command`` returns non-zero, the entire skill/agent/command fails to load. Use fallbacks like `command 2>/dev/null || echo "fallback"`.

## Python

Use [uv](https://docs.astral.sh/uv/) with inline dependencies (PEP 723). Run scripts with `uv run script.py`.

## hive-mind Session Files

The `.claude/hive-mind/sessions/` directory contains extracted session data. These files are gitignored.
