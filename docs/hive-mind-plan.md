# hive-mind Plan

A system for alignment researchers to contribute session learnings to a shared knowledge base.

## Core Principles

- **Storage over retrieval**: Capture now; improve retrieval later
- **Yankability**: Users can retract consent and remove their data
- **Human review**: All content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak
- **Local-first**: Provide immediate value through local knowledge before remote features

## Design Session Workflow

1. **During session**: Explore, test, discuss. Implement when appropriate.
2. **At session end**: Claude writes up the session file documenting what was tested and decided, then updates the plan (this file)
3. **Plan updates**: This file contains current design state and future plans, not reasoning (that lives in session files)

## Session Progress

### v0.1 Design & Implementation
- [x] [Session 0](sessions/session-0-initial-planning.md): Initial Planning
- [x] [Session 1](sessions/session-1-privacy-storage.md): Privacy & Storage Architecture
- [x] [Session 2](sessions/session-2-hook-behavior.md): Hook Behavior & User Prompting
- [x] Session 3: Plugin Naming
- [x] [Session 4](sessions/session-4-hook-output.md): Hook Output Testing
- [x] [Session 5](sessions/session-5-ideas-discussion.md): Ideas Discussion
- [x] [Session 6](sessions/session-6-setup-auth.md): First-Time Setup & Multi-Environment
- [x] [Session 7](sessions/session-7-typescript-migration.md): TypeScript/Bun Migration
- [x] [Session 8](sessions/session-8-jsonl-format.md): JSONL Format Deep Dive
- [x] Session 9: Local Extraction & Retrieval (design only)
- [x] Session 10A: Extraction Implementation
- [x] Session 10B: Local Retrieval (CLI commands, retrieval agent, field filtering, range reads)
- [x] Session 11: WorkOS Production & Signup Flow
- [ ] **Session 12: Convex Submission** ← NEXT (heartbeats, background upload, R2 storage)
- [ ] Session 13: Local Audit Server (view/audit sessions, manage submission status)
- [ ] Session 14: Testing Strategy
- [x] Session 15: User Communication Style (hook messages, error UX)

### v0.2 Design
- [ ] Processing Pipeline (Fly.io)
- [ ] Admin Web App

## Implementation Status

**Plugin skeleton**: `plugins/hive-mind/` - authentication flow implemented and tested. See code for details.

## v0.1 Architecture

### Technology Stack

| Component | Choice |
|-----------|--------|
| CLI | TypeScript + Bun (bundled to `plugins/hive-mind/cli.js`) |
| Web App | TanStack Start + React |
| Auth | WorkOS AuthKit (web) + device flow (CLI) |
| Backend | Convex |
| File Storage | Cloudflare R2 |
| Local Extraction | Deterministic code (no AI) |
| Retrieval | Local JSONL + scripts |

**Package structure**: All code lives in `hive-mind/` at repo root:
- `hive-mind/cli/` - CLI source
- `hive-mind/src/` - TanStack Start web app
- `hive-mind/convex/` - Convex backend functions

### Authentication

**Purpose**: WorkOS auth identifies users for session submission (v0.1) and gates access to shared hive-mind data (v0.2). It is NOT for repo access (the repo is public).

User runs `bun ~/.claude/plugins/hive-mind/cli.js login` → WorkOS device flow → tokens stored in `~/.claude/hive-mind/auth.json`. SessionStart hook auto-refreshes expired tokens.

**Credentials**: Client ID embedded in code (public). API key needed for Convex (secret, store securely). Currently using staging; switch to production for launch.

**Multi-environment**: Each machine independent (auth per-machine, transcripts per-machine, extracted sessions per-project).

### Plugin Installation (per-project)

- Plugin installed at project scope
- Plugin presence = participation
- Creates `.claude/hive-mind/` in project folder

### SessionStart Hook

Single hook handles auth check, session tracking, extraction, heartbeats, and submission. Must be idempotent (may run from parallel sessions, use last-write-wins).

**Current behavior** (implemented in TypeScript):
1. Check auth status, silently refresh if token expired
2. If not authenticated: display login command and optional shell alias setup
3. If authenticated: display "Logged in as {name}"

**Session 10A adds** (extraction):
1. Scan raw session files in `transcript_path` parent folder
2. For each session file, read first line of extracted file (if exists) to get metadata
3. Compare `rawMtime` in metadata to current file mtime → find new/modified sessions
4. For each relevant session:
   - Extract: sanitize (Secretlint), remove bloat, transform tool results
   - Write to `.claude/hive-mind/sessions/<id>.jsonl` with metadata first line
5. Log: "Extracted N new sessions"

**Session 11 adds** (submission, separate code section for easy parallel development):
- Heartbeat calls for all sessions
- 24h review period tracking
- Background upload trigger

**Hook data available:**
- `session_id`, `transcript_path`, `cwd`
- `source`: `startup` | `resume` | `compact`
- `transcript_path` parent folder contains all raw session JSONL files for the project

**Output format** (shows to user, not Claude):
```bash
echo '{"systemMessage": "Line 1\nLine 2\nLine 3"}'
```
Note: First line gets `SessionStart:startup says:` prepended by Claude Code.

### Local State

**Global** (`~/.claude/hive-mind/`):
- `auth.json` - JWT tokens from WorkOS (access_token, refresh_token, user info)

**Per-project** (`.claude/hive-mind/`):
- `checkout-id` - Random UUID per checkout (gitignored, so each worktree gets its own)
- `sessions/<session-id>.jsonl` - Self-contained extracted session:
  - Line 1: Metadata (extraction info, checkoutId, summary, message count, raw file mtime)
  - Lines 2+: Extracted message entries (sanitized, bloat removed)

No separate `state.json` or `index.md` - metadata lives in each session file's first line. This avoids git merge conflicts when syncing across machines.

User decides whether to gitignore `.claude/hive-mind/` (default: not gitignored, so users see what's created).

### Convex State

```
sessions:
  _id: auto-generated
  session_id: string
  user_id: string
  project: string
  line_count: number
  last_activity: timestamp  # from session content
  status: pending | submitted | excluded
  transcript_r2_key: string | null
```

Schema details subject to change during implementation.

### Convex API

```
POST session/heartbeat
  - Upsert session (create if new, update timestamp + line_count + status)
  - Called by SessionStart hook for all sessions (including excluded)

POST session/upload
  - Upload extracted transcript to R2
  - Called by background submission script
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

## v0.1 Session Details

### Session 8: JSONL Format (Completed)

See [claude-code-jsonl-format.md](claude-code-jsonl-format.md) for full reference.

**Key findings:**
- Entry types: `summary`, `user`, `assistant`, `system`, `file-history-snapshot`, `queue-operation`
- Conversation chain: `uuid` → `parentUuid` links; branches from `/rewind`
- **Summary bug**: ~80% of summaries contaminated from other sessions ([#2597](https://github.com/anthropics/claude-code/issues/2597)); only trust summaries where `leafUuid` exists in same file
- **Storage bloat**: Base64 content (56%), duplicate file reads, `originalFile` in edits → 92% reduction possible with cleaning

### Session 9: Local Extraction & Retrieval (Completed)

Design decisions documented in Session 10A/10B plans below.

### Session 10A: Extraction Implementation

Foundation for all downstream work. Creates the extracted session files that retrieval, submission, and audit all read from.

**Scope** (see `local/session-10a-extraction-plan.md`):
- Zod v4 schemas for JSONL parsing
- Secretlint sanitization
- Extraction logic (transform, sanitize, write)
- SessionStart hook integration (extraction only, not submission)
- Tests for extraction and sanitization

**Key outputs**:
- `cli/lib/schemas.ts`, `cli/lib/sanitize.ts`, `cli/lib/extraction.ts`
- `.claude/hive-mind/sessions/<id>.jsonl` files created on session start

### Session 10B: Local Retrieval

CLI tools and agent for searching past sessions. Depends on 10A (reads extracted files).

**Scope** (see `local/session-10b-retrieval-plan.md`):
- CLI commands: `index`, `read <id> [N]`
- Retrieval agent
- Tuning on real session data

### Session 11: WorkOS Production & Signup Flow

Switch from staging to production WorkOS app and create the signup callback page.

**Scope:**
- Switch WorkOS app to production environment
- Build callback page for invitation flow signups:
  - Short welcome message explaining what signing up enables
  - Redirect to GitHub repo for installation instructions
- Deploy the callback page
- Test end-to-end signup flow

**Acceptance criteria:**
- Production WorkOS app configured and working
- Callback page deployed and accessible
- New users see welcome message and can navigate to installation docs
- Existing CLI auth flow still works with production credentials

### Session 12: Convex Submission

Implement remote submission using existing Convex State and Convex API design (see v0.1 Architecture above):
- Heartbeat endpoint (upsert session metadata)
- Upload endpoint (R2 storage)
- Background submission script (delay after 24h review period)
- Status tracking (pending, submitted, excluded)
- Graceful degradation when Convex unavailable

**Privacy note:** Before authentication, only `checkoutId` is sent to Convex (for anonymous install tracking). No session data or other metadata is sent until the user authenticates. This allows counting plugin installations while respecting privacy.

### Session 13: Local Audit Server
- Local web server in CLI for viewing/auditing extracted sessions
- Manage submission status (exclude sessions before upload)
- Browse session content in browser

### Session 14: Testing
- Local testing approach
- Staging environment
- Dry-run mode

### Cross-Cutting
- Consent model if expanding beyond MATS
- Rollback procedure for leaked sensitive data

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

## v0.2 Session Details

- Fly.io machine setup and lifecycle
- Job state tracking in Convex
- Error handling and retries
- Admin web app design
- Queueing to limit concurrent processing jobs

## Open Issues

### Retrieval Agent Permissions Not Enforced

**Status**: Investigating

The `allowed-tools` restrictions in the retrieval skill (`plugins/hive-mind/skills/retrieval/SKILL.md`) are not being enforced. Agents can use any Bash command despite restrictions like:
```yaml
allowed-tools:
  - Bash(bun *cli.js index*)
  - Bash(bun *cli.js read*)
  - Bash(bun *cli.js grep*)
  - Bash(git log*)
  - Bash(git show*)
```

Observed violations include `cat`, `find`, `grep -r`, and `ls` commands that executed successfully and returned real output. This is a Claude Code behavior issue, not a hive-mind bug.

**Next steps**: Investigate how skill permissions are supposed to work, check if this is a known Claude Code issue.

### Subagent Extraction (Claude Code 2.1.0+)

Claude Code 2.1.0 changed subagent storage from standalone `agent-*.jsonl` files to `<session-id>/subagents/agent-*.jsonl` directories. Extraction updated to handle both formats. Currently extracting all agents flat to `.claude/hive-mind/sessions/agent-*.jsonl` - may want to preserve hierarchy later if needed for parent-child relationships.

### CLI Feature Gaps (from usage analysis)

Analysis of retrieval agent bash commands revealed patterns where the model used raw bash instead of CLI commands. See `docs/retrieval-agent-bash-commands.txt` for the raw data.

**Observed patterns:**
1. **Range reads**: Model tried `read <id> 150-200` but CLI only supports single entry reads
2. **Reading project files**: Model used `cat docs/*.md`, `grep -r` on project files to cross-reference session findings with actual documentation/code
3. **Finding files**: Model used `find` and `ls` to explore project structure

**Questions to resolve:**
- Should we add range read support (`read <id> N-M`)?
- Should retrieval agents be allowed to read non-session files (project docs, code)?
- If yes, should this be via raw file access or a new CLI command?
- How does this interact with the (currently broken) permissions system?

## Session 11: WorkOS Production & Signup Flow - Completed

**What was accomplished:**
- Restructured repository as bun monorepo: `web/` (TanStack Start) and `hive-mind/` (CLI) at root
- Built minimal web app: homepage, OAuth callback, welcome page with 4-step onboarding
- Integrated shadcn/ui components with Slate color scheme
- Implemented automatic dark mode via `prefers-color-scheme` media query
- Updated CLI config to use production WorkOS credentials by default
- Created Vercel deployment configuration
- Documented all development workflows

**Production Deployment Checklist:**

After merging to main, deploy to alignment-hive.com:

```bash
# 1. Run from repo root (NOT web/ directory)
vercel link
# Should create "alignment-hive" project

# 2. Add Vercel environment variables (via Dashboard)
# WorkOS (production):
#   WORKOS_CLIENT_ID=client_01KE10CZ6FFQB9TR2NVBQJ4AKV
#   WORKOS_API_KEY=<from WorkOS dashboard>
#   WORKOS_COOKIE_PASSWORD=<secure random 32+ chars>
#   WORKOS_REDIRECT_URI=https://alignment-hive.com/callback
# Convex (production):
#   CONVEX_DEPLOY_KEY=<from Convex dashboard>
#   CONVEX_DEPLOYMENT=<prod deployment name>
#   VITE_CONVEX_URL=https://<deployment>.convex.cloud

# 3. Deploy
git push origin main
# Vercel auto-deploys on push

# 4. Verify
# Visit https://alignment-hive.com and test sign-up flow
```

**Key architectural decisions:**
- CLI defaults to production credentials; local dev overrides with `HIVE_MIND_CLIENT_ID` env var
- Web app is production-only via Vercel; CLI runs locally on user machines
- All styling via CSS variables in `src/app.css` for easy theming
- OAuth callback intercepts response to redirect to `/welcome` instead of homepage

## Future Features

- User web dashboard for session management
- Yanking (user requests data deletion after submission)
- Granular admin access permissions
- Partial consent/redaction
- Windows support (if not easy to include in v0.1)
- Claude Code for web support

## Reference Documentation

- [Claude Code JSONL Format](claude-code-jsonl-format.md) - Internal format reference
- [claude-code-transcripts](https://github.com/simonw/claude-code-transcripts) - Simon Willison's transcript viewer
- [WorkOS CLI Auth](https://workos.com/docs/user-management/cli-auth)
- [Convex Custom JWT Auth](https://docs.convex.dev/auth/advanced/custom-jwt)
- [Convex R2 Component](https://www.convex.dev/components/cloudflare-r2)
- [Convex HTTP Actions](https://docs.convex.dev/functions/http-actions)
- [Fly.io Machines API](https://fly.io/docs/machines/api/)
