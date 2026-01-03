# hive-mind Plan

A system for MATS researchers to contribute session learnings to a shared knowledge base.

## Core Principles

- **Storage over retrieval**: Capture now; improve retrieval later
- **Yankability**: Users can retract consent and remove their data
- **Human review**: All content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak
- **Local-first**: Provide immediate value through local knowledge before remote features

## Design Session Workflow

1. **During session**: Explore, test, discuss. Confirm decisions before moving on
2. **At session end**: Claude writes up the session file documenting what was tested and decided, then updates the plan (this file)
3. **Plan updates**: This file contains current design state and future plans, not reasoning (that lives in session files)

## Session Progress

### v1 Design
- [x] [Session 0](sessions/session-0-initial-planning.md): Initial Planning
- [x] [Session 1](sessions/session-1-privacy-storage.md): Privacy & Storage Architecture
- [x] [Session 2](sessions/session-2-hook-behavior.md): Hook Behavior & User Prompting
- [x] Session 3: Plugin Naming
- [x] [Session 4](sessions/session-4-hook-output.md): Hook Output Testing
- [x] [Session 5](sessions/session-5-ideas-discussion.md): Ideas Discussion
- [ ] **Session 6: First-Time Setup & Multi-Environment** ← NEXT
- [ ] Session 7: JSONL Format Deep Dive
- [ ] Session 8: Local Extraction & Retrieval
- [ ] Session 9: Testing Strategy

### v1 Implementation
Starts after v1 design sessions complete.

### v2 Design
- [ ] Processing Pipeline (Fly.io)
- [ ] Admin Web App

## v1 Architecture

### Technology Stack

| Component | Choice |
|-----------|--------|
| Auth | WorkOS |
| Backend | Convex |
| File Storage | Cloudflare R2 |
| Local Extraction | Deterministic code (no AI) |
| Retrieval | Local JSONL + jq/scripts |

### Setup (one-time)

User runs login command → WorkOS device flow → JWT stored in `~/.claude/hive-mind/auth.json`

### Plugin Installation (per-project)

- Plugin installed at project scope
- Plugin presence = participation
- Creates `.claude/hive-mind/` in project folder

### SessionStart Hook

Single hook handles all session tracking, extraction, heartbeats, and submission. Must be idempotent (may run from parallel sessions, use last-write-wins).

**Data available:**
- `session_id`, `transcript_path`, `cwd`
- `source`: `startup` | `resume` | `compact`
- `transcript_path` parent folder contains all raw session JSONL files for the project

**Behavior:**
1. Scan raw session files in `transcript_path` parent folder
2. Compare to `state.json` → find untracked or modified sessions
3. For each relevant session:
   - Extract: sanitize (API key patterns etc.), remove bloat
   - Write to `.claude/hive-mind/sessions/<id>.jsonl`
   - Update `state.json` with extraction details
   - Update `sessions/index.md` with session description
4. Send heartbeats to Convex (last message timestamp from session content)
5. Display sessions < 24h (pending, can exclude via editing state.json)
6. For sessions > 24h: launch background script with 10-min delay
7. Background script checks for exclusion, uploads extracted JSONL if not excluded

**Output format** (shows to user, not Claude):
```bash
echo '{"systemMessage": "Line 1\nLine 2\nLine 3"}'
```
Note: First line gets `SessionStart:startup says:` prepended by Claude Code.

### Local State

**Global** (`~/.claude/hive-mind/`):
- `auth.json` - JWT token from WorkOS

**Per-project** (`.claude/hive-mind/`):
- `sessions/<session-id>.jsonl` - Extracted session (sanitized, bloat removed)
- `sessions/index.md` - Short descriptions for retrieval agent navigation
- `state.json` - Status per session: pending, submitted, excluded; extraction details

User decides whether to gitignore `.claude/hive-mind/` (may be useful for collaborators or multi-environment).

Session readiness determined from raw transcript file modified time.

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

Subagent reads `sessions/index.md` for navigation, uses jq/scripts to explore session JSONL files. Can get trajectory (truncated) then dive into details.

## v1 Open Questions

### Session 6: First-Time Setup
- Login flow implementation (WorkOS device flow)
- CLI tool vs slash commands for user actions
- Multi-environment handling (local + cloud VM)
- First-run experience and reminders

### Session 7: JSONL Format
- Full reverse-engineering of transcript format
- Summary entry structure and purpose
- Session description source (for display to user)
- Reference: https://github.com/simonw/claude-code-transcripts

### Session 8: Local Extraction & Retrieval
- Extraction format: thinner JSONL with sanitization
- Sanitization library for API key patterns
- `sessions/index.md` format
- Retrieval subagent design (jq, scripts, or custom tools)
- Guidance for navigating JSONL effectively

### Session 9: Testing
- Local testing approach
- Staging environment
- Dry-run mode

### Cross-Cutting
- Graceful degradation when Convex unavailable
- Consent model if expanding beyond MATS
- Rollback procedure for leaked sensitive data

## v2 Architecture

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

## v2 Open Questions

- Fly.io machine setup and lifecycle
- Job state tracking in Convex
- Error handling and retries
- Admin web app design
- Queueing to limit concurrent processing jobs

## Future Features

- User web dashboard for session management
- Yanking (user requests data deletion after submission)
- Granular admin access permissions
- Partial consent/redaction
- Windows support (if not easy to include in v1)
- Claude Code for web support

## Reference Documentation

- [WorkOS CLI Auth](https://workos.com/docs/user-management/sessions/cli-auth)
- [Convex Custom JWT Auth](https://docs.convex.dev/auth/advanced/custom-jwt)
- [Convex R2 Component](https://www.convex.dev/components/cloudflare-r2)
- [Convex HTTP Actions](https://docs.convex.dev/functions/http-actions)
- [Fly.io Machines API](https://fly.io/docs/machines/api/)
- [claude-code-transcripts](https://github.com/simonw/claude-code-transcripts) - JSONL format reference
