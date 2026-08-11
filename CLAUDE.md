# alignment-hive

Sub-`CLAUDE.md` files in `packages/hive-cli/`, `crates/`, and each `plugins/*` carry area-specific guidance. Web app local dev: `packages/web/README.md`.

## Running Scripts

From the repo root: `bun run --filter '*' <script>` for all workspaces, or `bun run --filter '<name>' <script>` for one. Workspaces without the script are skipped.

## Codebase Exploration

Always use `precis` for codebase exploration: `precis .` for an overview, `precis <dir>` to zoom in.
