# hive-mind

A system for alignment researchers to contribute session learnings to a shared knowledge base.

See [docs/hive-mind-plan.md](../../docs/hive-mind-plan.md) for detailed design and implementation plan.

## Development

The hive-mind package lives in `hive-mind/` at the repo root:
- `hive-mind/src/` - TanStack Start web app
- `hive-mind/cli/` - CLI source, bundled to `plugins/hive-mind/cli.js`
- `hive-mind/convex/` - Convex backend functions

```bash
cd hive-mind
bun install          # Install dependencies
bun run dev          # Run web app + Convex dev server
bun run lint         # Typecheck + ESLint
bun run test         # Run tests
bun run cli:build    # Bundle CLI to plugins/hive-mind/cli.js
```

To run CLI commands during development (from project root):
```bash
bun hive-mind/cli/cli.ts <command>
```

### Git hooks

A pre-commit hook auto-rebuilds the CLI when source changes. To enable:

```bash
git config core.hooksPath .githooks
```

User-facing CLI strings are centralized in `cli/lib/messages.ts` for easy review and updates.

### Regenerating extracted sessions

After schema or extraction changes, delete cached sessions and re-run extraction (from project root):

```bash
rm -rf .claude/hive-mind/sessions/*.jsonl
bun hive-mind/cli/cli.ts session-start
```
