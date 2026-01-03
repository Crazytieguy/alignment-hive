# hive-mind

A system for alignment researchers to contribute session learnings to a shared knowledge base.

See [docs/hive-mind-plan.md](../../docs/hive-mind-plan.md) for detailed design and implementation plan.

## Development

The CLI source lives in `hive-mind-cli/` at the repo root, bundled output goes to `plugins/hive-mind/cli.js`.

```bash
cd hive-mind-cli
bun install        # Install dependencies
bun run typecheck  # Type check
bun run build      # Bundle to plugins/hive-mind/cli.js
bun run dev <cmd>  # Run directly without bundling
```

A pre-commit hook auto-rebuilds when source changes. Run `scripts/install-hooks.sh` from the repo root to install it.

User-facing strings are centralized in `src/lib/messages.ts` for easy review and updates.
