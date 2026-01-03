# alignment-hive

Claude Code infrastructure for AI safety researchers.

## About This Repo

This is early-stage infrastructure. @README.md serves as the living plan document - it captures current priorities, next steps, and design decisions as they evolve.

## Development

### hive-mind CLI

The CLI source lives in `hive-mind-cli/`, bundled output goes to `plugins/hive-mind/cli.js`.

```bash
cd hive-mind-cli
bun install        # Install dependencies
bun run typecheck  # Type check
bun run build      # Bundle to plugins/hive-mind/cli.js
bun run dev <cmd>  # Run directly without bundling
```

A pre-commit hook auto-rebuilds when source changes. Run `scripts/install-hooks.sh` to install it.

User-facing strings are centralized in `src/lib/messages.ts` for easy review and updates.

## Updating the README

- Keep "Immediate Next Steps" current: move completed items out, add new ones as they emerge
- Update design notes as decisions are made or changed
- Add new sections as the project grows, but keep it scannable
- The README is the source of truth for "what are we doing next" - treat it as a working document, not a polished artifact
