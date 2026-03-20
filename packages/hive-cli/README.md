# hive-cli

CLI for alignment-hive session extraction, sharing, and management. Powers both the `hive` and `hive-mind` plugins.

## Development

**Important:** Always run commands from the monorepo root (`alignment-hive/`).

When committing changes, always run:
- `bun run --filter '@alignment-hive/hive-cli' test`
- `bun run --filter '@alignment-hive/hive-cli' lint`

Both must pass before committing.

**Important:** Never pipe test output (e.g., `bun test 2>&1 | head`). This causes the process to stall indefinitely. Always run tests without piping.

## Session Metadata

Keep session metadata minimal. Statistics should be computed on-the-fly during queries rather than stored. This reduces breaking changes and avoids requiring users to re-extract sessions.

## User-Facing Messages

All user-facing strings (CLI output, error messages, help text) should be defined in `src/lib/messages.ts`. This centralizes text for consistency and potential i18n.

## Re-extracting Sessions

To re-extract all sessions (e.g., after schema changes):
```bash
rm -rf .claude/hive-mind/sessions/
bun packages/hive-cli/src/hive-mind-cli.ts session-start
```

## Regenerating Snapshot Tests

The format tests use custom snapshot logic. To update snapshots:
```bash
UPDATE_SNAPSHOTS=1 bun run --filter '@alignment-hive/hive-cli' test
```

## Version Sync

When bumping the version in `package.json`, also bump `plugins/hive/cli-version` to match. The hive plugin uses this to download the correct binary for users.

The retrieval skill dynamically includes `--help` output. When CLI behavior changes, update the `--help` text in the command file and bump the plugin version.

## Environment Variables

| Variable | Description |
|----------|-------------|
| `HIVE_MIND_VERBOSE` | Set to `1` to show full error details in session-start hook output. By default, errors are summarized as a count. Only affects the session-start hook. |
| `HIVE_MIND_CLIENT_ID` | Override WorkOS client ID for hive-mind (for staging/testing). |
| `ALIGNMENT_HIVE_CLIENT_ID` | Override WorkOS client ID for hive (for staging/testing). |
| `ALIGNMENT_HIVE_CONVEX_URL` | Override Convex deployment URL (for local dev). Set in `.env.local`. |
| `DEBUG` | Set to `1` to enable debug logging. |

## Dev Binary

Build and run the dev binary:
```bash
bun run --filter '@alignment-hive/hive-cli' build:dev
.dev/hive <command>
```

The dev binary embeds `ALIGNMENT_HIVE_DEV=1` at build time via `--define`, which causes `loadEnvFiles()` to load `.env` and `.env.local` from CWD. This gives it staging defaults (from `.env`) and per-dev overrides like `ALIGNMENT_HIVE_CONVEX_URL` (from `.env.local`). The production binary skips env file loading entirely and uses hardcoded production defaults.

Example commands:
```bash
.dev/hive upload list          # List sessions with status
.dev/hive upload review        # Open local review UI
.dev/hive upload exclude <id>  # Exclude a session
.dev/hive upload snooze 24h    # Pause uploads
.dev/hive upload send          # Upload all eligible sessions
.dev/hive upload send <id>     # Upload a specific session
.dev/hive consent status       # Check consent status
```

## Local Development with Staging Auth

Staging defaults (`ALIGNMENT_HIVE_CLIENT_ID`, `ALIGNMENT_HIVE_AUTH_FILE`) are in the checked-in root `.env` file. These are only loaded by the dev binary (see above).

Per-dev overrides (e.g. `ALIGNMENT_HIVE_CONVEX_URL`) go in root `.env.local`, which is created by `bash scripts/setup-web.sh`.
