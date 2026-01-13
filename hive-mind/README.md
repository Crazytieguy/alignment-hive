# hive-mind

A system for alignment researchers to extract and share session learnings across the community.

## Development

When committing changes, always run:
- `bun test`
- `bun run lint`

Both must pass before committing.

## Session Metadata

Keep session metadata minimal. Statistics should be computed on-the-fly during queries rather than stored. This reduces breaking changes and avoids requiring users to re-extract sessions.

## Re-extracting Sessions

To re-extract all sessions (e.g., after schema changes):
```bash
rm -rf .claude/hive-mind/sessions/
bun hive-mind/cli/cli.ts session-start
```

## Regenerating Snapshot Tests

The format tests use custom snapshot logic. To update snapshots:
```bash
UPDATE_SNAPSHOTS=1 bun test
```

## Skill and CLI Sync

The retrieval skill dynamically includes `--help` output. When CLI behavior changes, update the `--help` text in the command file and bump the plugin version.
