# alignment-hive

Claude Code infrastructure for AI safety researchers.

## About This Repo

@README.md explains what this is. Keep it up to date as the project evolves.

**Important:** The installation instructions in `README.md` and `packages/web/src/routes/_authenticated/welcome.tsx` must stay in sync. When updating one, update the other.

This is a **bun monorepo**:
- `packages/web/` - TanStack Start web app (alignment-hive.com)
- `packages/hive-mind-cli/` - CLI for session extraction
- `packages/shared/` - Shared code (schemas, parsing)
- `plugins/` - Plugin distributions

## Working on the Code

**For web app**: Read [packages/web/README.md](packages/web/README.md) for local development setup

**For CLI**: Read [packages/hive-mind-cli/CLAUDE.md](packages/hive-mind-cli/CLAUDE.md) for development guidelines. Run CLI commands from the project root: `bun packages/hive-mind-cli/src/cli.ts <command>`

## Running Scripts

Run workspace scripts from the repo root using `bun run --filter`:

```bash
# All workspaces
bun run --filter '*' lint
bun run --filter '*' build
bun run --filter '*' format

# Specific workspace
bun run --filter '@alignment-hive/hive-mind-cli' test
bun run --filter '@alignment-hive/hive-mind-cli' lint
bun run --filter '@alignment-hive/web' lint
```

Workspaces without the script are skipped (no error).

For workspace-specific tasks like dev servers:
```bash
cd packages/web && bun run dev
```

## Adding New Plugins

New plugins must be registered in `.claude-plugin/marketplace.json` to appear in the marketplace. Add an entry with `name`, `source`, and `description`. The `name` must match the plugin's folder name under `plugins/` (e.g., `"name": "autopilot"` for `plugins/autopilot/`).

## Plugin Versioning

When updating plugin content (skills, commands, hooks, etc.), you must bump the version in the plugin's `plugin.json` for users to receive the update. The auto-update system compares installed versions with marketplace versions - without a version bump, changes won't propagate to users.

Plugin locations:
- `plugins/autopilot/.claude-plugin/plugin.json`
- `plugins/github-action/.claude-plugin/plugin.json`
- `plugins/hive-mind/.claude-plugin/plugin.json`
- `plugins/mats/.claude-plugin/plugin.json`
- `plugins/llms-fetch-mcp/.claude-plugin/plugin.json`
- `plugins/remote-kernels/.claude-plugin/plugin.json`

For the mats plugin specifically:
- **Minor version bump** (e.g., 0.1.x → 0.2.0): New best practices content - users will be prompted to review
- **Patch version bump** (e.g., 0.1.9 → 0.1.10): Bug fixes, typos, or other changes - users won't be re-prompted
- **Update README.md** when adding or significantly changing mats plugin skills/commands

**Auto-expanding bash commands fail hard.** If `!`command`` returns non-zero, the entire skill/agent/command fails to load. Use fallbacks like `command 2>/dev/null || echo "fallback"`.

## Python

Use [uv](https://docs.astral.sh/uv/) with inline dependencies (PEP 723). Run scripts with `uv run script.py`.

## hive-mind Session Files

The `.claude/hive-mind/sessions/` directory contains extracted session data. These files are gitignored.

## Running Commands

Run scripts via `bun run --filter <workspace> <script>`. Available scripts vary by workspace - see "Running Scripts" section above.

**Ad-hoc scripts:** Only `/tmp/claude-execution-allowed/alignment-hive/` is approved for ad-hoc scripts. JavaScript/TypeScript scripts run with `bun run /tmp/claude-execution-allowed/alignment-hive/<script-name>`. Bash scripts run with `bash /tmp/claude-execution-allowed/alignment-hive/<script-name>`.

**Bash operations:**

Complex bash syntax is hard for Claude Code to permission correctly. Keep commands simple.

Simple operations are fine: `|`, `||`, `&&`, `>` redirects.

For bulk operations on multiple files, use xargs:
- Plain: `ls *.md | xargs wc -l`
- With placeholder: `ls *.md | xargs -I{} head -1 {}`

For string interpolation (`$()`, backticks, `${}`), heredocs, loops, or advanced xargs flags (`-P`, `-L`, `-n`), write a script in `/tmp/claude-execution-allowed/alignment-hive/` instead.

**Patterns:**
- File creation: Write tool, not `cat << 'EOF' > file`
- Env vars: `export VAR=val && command`, not `VAR=val command` or `env VAR=val command`
- Bulk operations: `ls *.md | xargs wc -l`, not `for f in *.md; do cmd "$f"; done`
- Parallel/batched xargs: script, not `xargs -P4` or `xargs -L1`
- Per-item shell logic: script, not `xargs sh -c '...'`
- Git: `git <command>`, not `git -C <path> <command>` (breaks permissions)

If a command that should be allowed is denied, or if project structure changes significantly, ask about running `/autopilot:setup` to update settings.

## Codebase exploration

Always use `precis` for codebase exploration. Run `precis .` for a full overview, or `precis src/some/directory` to zoom into a specific area.

