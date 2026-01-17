# hive-mind

A system for alignment researchers to extract and share session learnings across the community.

## Development

**Important:** Always run commands from the monorepo root (`alignment-hive/`).

When committing changes, always run:
- `bun run --filter '@alignment-hive/hive-mind' test`
- `bun run --filter '@alignment-hive/hive-mind' lint`

Both must pass before committing.

**Important:** Never pipe test output (e.g., `bun test 2>&1 | head`). This causes the process to stall indefinitely. Always run tests without piping.

## Session Metadata

Keep session metadata minimal. Statistics should be computed on-the-fly during queries rather than stored. This reduces breaking changes and avoids requiring users to re-extract sessions.

## User-Facing Messages

All user-facing strings (CLI output, error messages, help text) should be defined in `cli/lib/messages.ts`. This centralizes text for consistency and potential i18n.

## Re-extracting Sessions

To re-extract all sessions (e.g., after schema changes):
```bash
rm -rf .claude/hive-mind/sessions/
bun hive-mind/cli/cli.ts session-start
```

## Regenerating Snapshot Tests

The format tests use custom snapshot logic. To update snapshots:
```bash
UPDATE_SNAPSHOTS=1 bun run --filter '@alignment-hive/hive-mind' test
```

## Skill and CLI Sync

The retrieval skill dynamically includes `--help` output. When CLI behavior changes, update the `--help` text in the command file and bump the plugin version.

## Local Development with Staging Auth

To test the CLI against the staging WorkOS environment instead of production, copy the project root `.env.example` to `.env.local`:
```bash
cp .env.example .env.local
```

This sets `HIVE_MIND_CLIENT_ID` to use the staging WorkOS client instead of production.
