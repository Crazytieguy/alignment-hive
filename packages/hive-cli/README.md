# hive-cli

CLI for alignment-hive session sharing and management. Powers the `hive` plugin.

## Development

Before committing, `bun run --filter '@alignment-hive/hive-cli' test` and `bun run --filter '@alignment-hive/hive-cli' lint` must both pass. Never pipe test output (e.g. `bun test 2>&1 | head`): the process stalls indefinitely.

## User-Facing Messages

User-facing strings (CLI output, errors, help) live in `src/lib/messages.ts`.

## Local Transcript Inspection

`hive local` (`sessions`, `outline`, `show`, `grep`) reads local Claude Code transcripts without changing sharing settings or the transcript-directory registry. Its one help page, `localHelp` in `src/lib/messages.ts`, is the reference.

It reads transcripts only through `@alignment-hive/session-data` (`parseTranscript`; hiding and selection in `noise.ts`) and resolves locators only through `src/lib/locators.ts`, which `hive review render` shares, so both print and accept the same entry numbers.

## Version Sync

For a release, bump `plugins/hive/cli-version` together with `package.json` (`bootstrap.sh` downloads the binary for that version), then bump the plugin version so the marketplace ships the new file. The retrieval skill injects `hive local --help` whole, so a change to that page is a change to the skill.

## Dev Binary

`bun run --filter '@alignment-hive/hive-cli' build:dev` writes `.dev/hive` at the repo root. It is compiled with `ALIGNMENT_HIVE_DEV=1`, so it loads `.env.local` (per-dev overrides such as `ALIGNMENT_HIVE_CONVEX_URL`; created by `bash scripts/setup-web.sh`) and then `.env` (checked-in staging defaults: `ALIGNMENT_HIVE_CLIENT_ID`, `ALIGNMENT_HIVE_AUTH_FILE`, `ALIGNMENT_HIVE_URL`) from the cwd. The production binary never reads env files. `DEBUG=1` enables debug logging in either.
