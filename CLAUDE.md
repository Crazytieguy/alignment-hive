# alignment-hive

Claude Code infrastructure for AI safety researchers.

## About This Repo

@README.md explains what this is. Keep it up to date as the project evolves.

This is a **bun monorepo**:
- `web/` - TanStack Start web app (alignment-hive.com)
- `hive-mind/` - CLI for session extraction
- `plugins/` - Plugin distributions

## Working on the Code

**For web app**: Read [web/README.md](web/README.md) for local development setup

**For CLI**: Read [hive-mind/CLAUDE.md](hive-mind/CLAUDE.md) for development guidelines

## Plugin Versioning

When updating plugin content (skills, commands, hooks, etc.), you must bump the version in `plugin.json` for users to receive the update. The auto-update system compares installed versions with marketplace versions - without a version bump, changes won't propagate to users.

## Python

Use [uv](https://docs.astral.sh/uv/) with inline dependencies (PEP 723). Run scripts with `uv run script.py`.

## The "Search" tool, and scanning the code in other ways

Make sure to ignore `.claude/hive-mind/sessions`, it contains the equivalent of long logs which will spam the context for you and any subagent you spawn.