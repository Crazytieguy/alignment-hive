# Session 7: TypeScript/Bun Migration

**Date**: 2026-01-03

## Goal

Replace bash scripts with TypeScript for better maintainability as the CLI grows in complexity.

## Decisions

### Why TypeScript + Bun

- Native TypeScript support without build step in development
- Type safety for JSON handling (sessions, auth tokens)
- Growing complexity: extraction, Convex calls, background jobs
- More contributors familiar with TS than advanced bash

### Distribution Strategy

**Bundling** (not compilation):
- `bun build` bundles to single `cli.js` file (~12KB)
- Committed to git, updates seamlessly with plugin
- Requires Bun runtime (users install once)

### CLI Structure

```
hive-mind-cli/           # Source (not distributed)
├── src/
│   ├── cli.ts           # Entry point
│   ├── commands/        # login.ts, session-start.ts
│   └── lib/             # auth.ts, config.ts, messages.ts, output.ts
├── package.json
└── tsconfig.json

plugins/hive-mind/       # Distributed
├── cli.js               # Bundled CLI
├── scripts/bootstrap.sh # Checks for Bun, runs CLI
└── hooks/hooks.json
```

### Cross-Platform

- Shell wrapper checks for Bun, shows install instructions if missing
- Windows users need Git Bash or Bun in PATH (deferred to v2 if issues arise)

### CLI Alias

Print copy-pasteable alias command when not logged in:
```
echo "alias hive-mind='bun ~/.../cli.js'" >> ~/.zshrc && source ~/.zshrc
```
Respects user autonomy (no automatic profile modification).

### CI/CD

- Pre-commit hook auto-rebuilds when `hive-mind-cli/src/` changes
- CI checks bundle is up to date (conditional on CLI file changes)
- Install hooks: `scripts/install-hooks.sh`

## Implementation Notes

### WorkOS SDK Decision

Evaluated but removed. Device authorization flow only needs client_id (public), not API key. Raw fetch is simpler and avoids unnecessary dependency.

### Centralized Strings

All user-facing messages in `src/lib/messages.ts` for:
- Easy review of communication style
- Consistent updates
- Session 11 will refine these

### Hook Output

Using `JSON.stringify({ systemMessage: "..." })` for robust escaping. First line gets "SessionStart:startup says:" prepended by Claude Code.

## Files Created/Modified

- `hive-mind-cli/` - New TypeScript source directory
- `plugins/hive-mind/cli.js` - Bundled CLI (replaces dist/)
- `plugins/hive-mind/scripts/bootstrap.sh` - Updated for new path
- `.github/workflows/ci.yml` - Conditional CI for CLI changes
- `scripts/install-hooks.sh` - Git hooks installer
- Deleted: `plugins/hive-mind/scripts/{login.sh,session-start.sh,config.sh}`, `plugins/hive-mind/dist/`
