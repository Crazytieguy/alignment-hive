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

All user-facing strings (CLI output, error messages, help text) should be defined in `cli/lib/messages.ts`. This centralizes text for consistency and potential i18n.

## Re-extracting Sessions

To re-extract all sessions (e.g., after schema changes):
```bash
rm -rf .claude/hive-mind/sessions/
bun packages/hive-cli/src/cli.ts session-start
```

## Regenerating Snapshot Tests

The format tests use custom snapshot logic. To update snapshots:
```bash
UPDATE_SNAPSHOTS=1 bun run --filter '@alignment-hive/hive-cli' test
```

## Skill and CLI Sync

The retrieval skill dynamically includes `--help` output. When CLI behavior changes, update the `--help` text in the command file and bump the plugin version.

## Environment Variables

| Variable | Description |
|----------|-------------|
| `HIVE_MIND_VERBOSE` | Set to `1` to show full error details in session-start hook output. By default, errors are summarized as a count. Only affects the session-start hook. |
| `HIVE_MIND_CLIENT_ID` | Override WorkOS client ID for hive-mind (for staging/testing). |
| `ALIGNMENT_HIVE_CLIENT_ID` | Override WorkOS client ID for hive (for staging/testing). |
| `DEBUG` | Set to `1` to enable debug logging. |

## Dev Binary

Build the hive binary for local testing:
```bash
bun run --filter '@alignment-hive/review-app' build && bun build --compile packages/hive-cli/src/hive-cli.ts --outfile .dev/hive
```

The dev environment (set up by `dev-env.sh`) puts `.dev/` on PATH, so `hive` runs the dev binary. Test commands:
```bash
hive upload list          # List sessions with status
hive upload review        # Open local review UI
hive upload exclude <id>  # Exclude a session
hive upload snooze 24h    # Pause uploads
hive upload now           # Upload immediately
hive consent status       # Check consent status
```

## Local Development with Staging Auth

Staging defaults (`HIVE_MIND_CLIENT_ID`, `HIVE_MIND_AUTH_FILE`) are in the checked-in root `.env` file — no setup needed.

The CLI loads `.env` and `.env.local` from CWD on startup (via `loadEnvFiles()`), so this works both when running with bun and as a compiled binary. Per-dev overrides (e.g. `ALIGNMENT_HIVE_CONVEX_URL`) go in root `.env.local`, which is created by `bash scripts/setup-web.sh`.
