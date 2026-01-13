# hive-mind CLI

The hive-mind CLI enables Claude Code users to extract and submit session learnings to the alignment-hive knowledge base.

## What it does

- **Session Extraction**: Automatically extracts Claude Code session information
- **Local Review**: Users review extractions before submission (24-hour window)
- **Knowledge Submission**: Contributes sessions to shared knowledge base for other researchers
- **Privacy Controls**: Users control what gets shared and can retract submissions

## Development

### Setup

From the repository root:

```bash
# Install dependencies for entire workspace
bun install

# Navigate to CLI directory
cd hive-mind
```

### Available Commands

```bash
bun run test         # Run CLI tests
bun run lint         # Type check and ESLint
bun run cli:build    # Bundle CLI to ../plugins/hive-mind/cli.js
```

### Running CLI During Development

From the project root:

```bash
bun hive-mind/cli/cli.ts <command> [args]
```

Examples:

```bash
bun hive-mind/cli/cli.ts login
bun hive-mind/cli/cli.ts extract
bun hive-mind/cli/cli.ts retrieve --query "authentication"
```

### Environment Variables

Set `HIVE_MIND_CLIENT_ID` to override the WorkOS client ID (useful for local testing with staging):

```bash
# Use staging credentials locally
export HIVE_MIND_CLIENT_ID=client_01KE10CYZ10VVZPJVRQBJESK1A
bun hive-mind/cli/cli.ts login
```

The default is production client ID (configured in `cli/lib/config.ts`).

### User-Facing Messages

All user-facing strings are centralized in `cli/lib/messages.ts`. When updating:

1. Edit messages in `cli/lib/messages.ts`
2. Run `bun run cli:build` to rebundle
3. Bump version in `plugins/hive-mind/plugin.json` for auto-update to users

### Git Hooks

To automatically rebuild the CLI on commit:

```bash
git config core.hooksPath .githooks
```

### Regenerating Sessions

After schema or extraction logic changes, re-extract all sessions:

```bash
# From project root
rm -rf .claude/hive-mind/sessions/
bun hive-mind/cli/cli.ts session-start
```

## Architecture

- **CLI**: TypeScript with Bun runtime, bundles to `plugins/hive-mind/cli.js`
- **Auth**: WorkOS device authorization flow
- **Storage**: Local `~/.claude/hive-mind/` directory
- **Backend**: Convex serverless functions
- **Web Integration**: Connects with web app at alignment-hive.com

## File Structure

- `cli/` - CLI source code
  - `cli.ts` - CLI entry point
  - `commands/` - Command implementations
  - `lib/` - Shared utilities
  - `tests/` - Test suite
- `CLAUDE.md` - Development guidelines (auto-loads this README)
