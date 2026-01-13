# alignment-hive

Claude Code infrastructure for AI safety researchers.

## About This Repo

@README.md explains what this is and how to use it. Keep it up to date as the project evolves.

## hive-mind

The hive-mind source code (CLI and web app) lives in `hive-mind/` at the repo root. The `plugins/hive-mind/` directory is just the plugin distribution (bundled CLI, plugin.json, etc.).

**Before working on hive-mind or using its CLI, read [hive-mind/CLAUDE.md](hive-mind/CLAUDE.md).** It contains essential development instructions including how to run the CLI, tests, and linting. Also update [plugins/hive-mind/README.md](plugins/hive-mind/README.md) as needed.

## Plugin Versioning

When updating plugin content (skills, commands, hooks, etc.), you must bump the version in `plugin.json` for users to receive the update. The auto-update system compares installed versions with marketplace versions - without a version bump, changes won't propagate to users.

## Python

Use [uv](https://docs.astral.sh/uv/) with inline dependencies (PEP 723). Run scripts with `uv run script.py`.

## The "Search" tool, and scanning the code in other ways

Make sure to ignore `.claude/hive-mind/sessions`, it contains the equivalent of long logs which will spam the context for you and any subagent you spawn.