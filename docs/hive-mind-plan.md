# hive-mind Plan

A system for alignment researchers to contribute session learnings to a shared knowledge base.

## Current State

**v0.1 shipped and in use.** The plugin extracts session data locally, supports retrieval via CLI commands, and uploads sessions to Convex after a 24-hour review period.

## Core Principles

- **Storage over retrieval**: Capture now; improve retrieval later
- **Yankability**: Users can retract consent and remove their data
- **Human review**: All content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak
- **Local-first**: Provide immediate value through local knowledge before remote features

## v0.1 Architecture

### Technology Stack

| Component | Choice |
|-----------|--------|
| CLI | TypeScript + Bun (bundled to `plugins/hive-mind/cli.js`) |
| Web App | TanStack Start + React |
| Auth | WorkOS AuthKit (web) + device flow (CLI) |
| Backend | Convex |
| File Storage | Convex built-in storage |
| Local Extraction | Deterministic code (no AI) |
| Retrieval | Local JSONL + scripts |

**Package structure**: Bun monorepo at repo root:
- `hive-mind/cli/` - CLI source (bundled to `plugins/hive-mind/cli.js`)
- `web/` - TanStack Start web app
- `web/convex/` - Convex backend functions

### Authentication

User runs `bun ~/.claude/plugins/hive-mind/cli.js login` → WorkOS device flow → tokens stored in `~/.claude/hive-mind/auth.json`. SessionStart hook auto-refreshes expired tokens.

### Plugin Installation (per-project)

- Plugin installed at project scope
- Plugin presence = participation
- Creates `.claude/hive-mind/` in project folder

### SessionStart Hook

Single hook handles auth check, session tracking, extraction, heartbeats, and submission. Must be idempotent (may run from parallel sessions, use last-write-wins).

**Behavior:**
1. Check auth status, silently refresh if token expired
2. If not authenticated: display login command and optional shell alias setup
3. If authenticated: display "Logged in as {name}"
4. Scan raw session files and extract new/modified sessions
5. Heartbeat calls for all sessions
6. Auto-upload eligible sessions (24h review period passed)

### Local State

**Global** (`~/.claude/hive-mind/`):
- `auth.json` - JWT tokens from WorkOS (access_token, refresh_token, user info)

**Per-project** (`.claude/hive-mind/`):
- `checkout-id` - Random UUID per checkout (gitignored, so each worktree gets its own)
- `sessions/<session-id>.jsonl` - Self-contained extracted session:
  - Line 1: Metadata (extraction info, checkoutId, summary, message count, raw file mtime)
  - Lines 2+: Extracted message entries (sanitized, bloat removed)

### Convex API

```
sessions.heartbeatSession
  - Upsert session metadata (sessionId, checkoutId, project, lineCount, parentSessionId)
  - Called by SessionStart hook for authenticated users

sessions.generateUploadUrl
  - Get pre-signed URL for Convex storage upload
  - Validates session exists and belongs to user

sessions.saveUpload
  - Record storage ID after successful upload
  - Marks session as uploaded

sessions.upsertCheckout
  - Track checkout IDs for anonymous install telemetry
  - Called on every session start (no auth required)
```

### Local Retrieval

Retrieval agent uses CLI commands:
- `hive-mind index` - List sessions with metadata (datetime, message count, summary, commit hashes)
- `hive-mind grep <pattern>` - Search across sessions (supports `-i`, `-c`, `-l`, `-m N`, `-C N` flags)
- `hive-mind read <id>` - Scan session entries (truncated for quick scanning)
- `hive-mind read <id> N` - Get full content for specific entry N
- `hive-mind read <id> N -C/-B/-A` - Get entry N with surrounding context entries
- Agent can also use `git log` and `git show <hash>` to correlate commits with sessions

Workflow: Use grep for specific terms, index for browsing, read for scanning sessions, read with entry number for details.

## v0.2 Architecture

### Remote Processing

Convex triggers Fly.io machine (spin up per job) to run Claude Code for indexing and cross-session insights. Results stored in R2, job state tracked in Convex.

**Shared secret setup:**
```bash
openssl rand -hex 32
```
- Convex: `FLY_ACTION_SECRET`
- Fly.io: `CONVEX_ACTION_SECRET`

**Processing hierarchy:** raw extraction → indexing → cross-session insights/reindexing

### Remote Retrieval

Subagent fetches from R2 via pre-signed URLs from Convex. Merges local + remote knowledge.

### Admin Web App

For processing pipeline management:
- Review index/content updates
- Manage processing jobs and versions
- View statistics
- Browse uploaded sessions

### v0.2 Tasks

- Fly.io machine setup and lifecycle
- Job state tracking in Convex
- Error handling and retries
- Admin web app design
- Queueing to limit concurrent processing jobs

## Backlog

### Future Features

- User web dashboard for session management
- Yanking (user requests data deletion after submission)
- Granular admin access permissions
- Partial consent/redaction
- Windows support
- Claude Code for web support
- Local audit server (browse/audit sessions before upload)

### Future Optimization Ideas

**Incremental extraction**: Currently we read and parse the entire raw session file when extraction is needed. For large sessions (100MB+), this takes 1+ seconds. Could track extraction progress in metadata and only process new entries since last extraction.

**Multi-hook extraction**: Currently extraction only runs in SessionStart hook. Running it in other hooks (Stop, compact) would spread the work and reduce the burst of extraction on session start.

**Background parsing with error reporting**: Move parsing entirely to background process. Store schema errors somewhere (local file or server) for later retrieval instead of blocking on parse-only check.

## Reference Documentation

- [Claude Code JSONL Format](claude-code-jsonl-format.md) - Internal format reference
- [claude-code-transcripts](https://github.com/simonw/claude-code-transcripts) - Simon Willison's transcript viewer
- [WorkOS CLI Auth](https://workos.com/docs/user-management/cli-auth)
- [Convex Custom JWT Auth](https://docs.convex.dev/auth/advanced/custom-jwt)
- [Convex R2 Component](https://www.convex.dev/components/cloudflare-r2)
- [Convex HTTP Actions](https://docs.convex.dev/functions/http-actions)
- [Fly.io Machines API](https://fly.io/docs/machines/api/)
