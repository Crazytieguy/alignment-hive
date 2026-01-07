# hive-mind

## Development

When committing changes, always run:
- `bun test`
- `bun run lint`

Both must pass before committing.

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
