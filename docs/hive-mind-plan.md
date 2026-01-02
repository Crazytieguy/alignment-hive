# hive-mind Plan

A system for MATS researchers to contribute session learnings to a shared knowledge base.

## Core Principles

- **Storage over retrieval**: Capture now; improve retrieval later
- **Yankability**: Users can retract consent and remove their data
- **Human review**: All content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak

## Session Progress

- [x] [Session 0](sessions/session-0-initial-planning.md): Initial Planning
- [x] [Session 1](sessions/session-1-privacy-storage.md): Privacy & Storage Architecture
- [x] [Session 2](sessions/session-2-hook-behavior.md): Hook Behavior & User Prompting
- [x] Session 3: Plugin Naming (hive-mind)
- [ ] **Session 4: Hook Output Testing** ← NEXT
- [ ] Session 5: Ideas Discussion
- [ ] Session 6: First-Time Setup & Multi-Environment
- [ ] Session 7: JSONL Format Deep Dive
- [ ] Session 8: Processing Pipeline (content format + index + GitHub Action)
- [ ] Session 9: Retrieval Subagent
- [ ] Session 10: Testing Strategy

## Architecture

```
SETUP (one-time)
  User runs login → Stytch device flow → JWT stored in ~/.claude/alignment-hive/
  Stytch webhook on signup → Convex → GitHub App invites to repo

PLUGIN INSTALLATION (per-project)
  User installs plugin at project scope
  Plugin presence = participation

SESSION TRACKING (continuous)
  Stop hook → Convex heartbeat (upsert session_id, timestamp, line_count)
  SessionStart hook → register session, check for pending/ready sessions

SUBMISSION FLOW
  SessionStart checks local session files:
    - Sessions < 24h old: display to user (can exclude)
    - Sessions > 24h old: queue for submission
  Background script (10 min delay) → final opt-out window → upload if not excluded

TRANSCRIPT UPLOAD
  Background script reads JWT → uploads transcript to Convex
        │
        ▼
CONVEX BACKEND
  Validates JWT (Stytch JWKS) → stores in R2 → records metadata
  Triggers GitHub Action via repository_dispatch
        │
        ▼
GITHUB ACTION
  Auth: shared secret → fetches transcript → runs Claude Code
  Creates PR with extracted knowledge (or rejects if PII/not useful)
  Uploads processing session to R2 for admin review
        │
        ▼
HUMAN REVIEW
  Reviewer merges PR → marketplace updates → users get new content

RETRIEVAL
  Subagent searches index → returns context to main agent
```

## Technology Decisions

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Auth | Stytch | First-class CLI device flow, JWT-based, invite-only built-in |
| Backend | Convex | Real-time, supports custom JWT auth for Stytch |
| File Storage | Cloudflare R2 | S3-compatible, zero egress, direct access for yankability |
| Processing | GitHub Actions | Free CI minutes, Claude Code support |
| Distribution | Git + plugin marketplace | Auto-updates via marketplace |
| GitHub Action Auth | Shared secret | Convex-recommended for external services |
| Repo Invitations | GitHub App | More secure than user OAuth tokens |
| Label Generation | Derived from content | Reduces friction; AI extracts labels, not user input |
| Plugin Scope | Project-level | Installed per-project; plugin presence = participation |
| Session Detection | Stop hook heartbeat | Reliable; fires after every Claude response |
| Submission Trigger | SessionStart hook | Checks for ready sessions; client controls timing |
| Submission Delay | 24h inactivity + 10min final window | Handles long sessions; gives opt-out opportunity |

## Hooks

| Hook | Purpose |
|------|---------|
| Stop | Heartbeat to Convex (upsert session_id, timestamp, line_count) |
| SessionStart | Register session, display pending sessions, submit ready sessions, show reminders |

### Hook Data Available

**SessionStart:**
- `session_id`, `transcript_path`, `cwd`
- `source`: `startup` | `resume` | `compact`

**Stop:**
- `session_id`, `transcript_path`, `cwd`
- `stop_hook_active`, `permission_mode`

### SessionStart Behavior

1. Check local session files for readiness (based on file modified time)
2. Display sessions < 24h (pending, can exclude)
3. For sessions > 24h: launch background script with 10-min delay
4. Background script checks for exclusion, uploads if not excluded

## State Management

### Local State (source of truth)

Location: `~/.claude/alignment-hive/`

| Data | Purpose |
|------|---------|
| `auth.json` | JWT token from Stytch |
| `sessions/<session-id>.json` | Status: pending, submitted, excluded |

Session readiness determined from local transcript file modified time (no Convex query needed).

### Convex State (replicated for stats/debugging)

```
sessions:
  _id: auto-generated
  session_id: string        # from Claude Code
  user_id: string
  project: string
  line_count: number
  last_activity: timestamp
  status: pending | submitted | excluded
  transcript_r2_key: string | null
```

Note: Schema details subject to change during implementation.

## Convex API

```
POST session/heartbeat
  - Upsert session (create if new, update timestamp + line_count)
  - Called by Stop hook

POST session/upload
  - Upload transcript to R2
  - Trigger processing pipeline
  - Called by background submission script

POST session/exclude
  - Record exclusion (for stats)
  - Called when user excludes a session
```

## Open Questions

### Session 4: Hook Output Testing
- How to display info to user without polluting Claude's context?
- Test: plain stdout, JSON with systemMessage, suppressOutput
- Test: slash command with disable-model-invocation

### Session 5: Ideas Discussion
- Ideas from time away from computer
- Any architectural changes needed

### Session 6: First-Time Setup
- Login flow implementation
- CLI tool vs slash commands for user actions
- Multi-environment handling (local + cloud VM)
- First-run experience and reminders

### Session 7: JSONL Format
- Full reverse-engineering of transcript format
- Summary entry structure and purpose
- Session description source (for display to user)

### Session 8: Processing Pipeline
- Content format and metadata extraction
- Index design (monolithic vs per-session, format)
- GitHub Action implementation
- PR format, rejection criteria

### Session 9: Retrieval
- Subagent trigger conditions
- Tools: grep, jq, custom?
- Context amount to return

### Session 10: Testing
- Local testing approach
- Staging branch/repo
- Dry-run mode

### Cross-Cutting
- Consent model if expanding beyond MATS
- Rollback procedure for leaked sensitive data
- Plugin auto-update verification

## Implementation Notes

### Shared Secret Setup
```bash
openssl rand -hex 32
```
- Convex: Dashboard → Settings → Environment Variables → `GITHUB_ACTION_SECRET`
- GitHub: Repo → Settings → Secrets → Actions → `CONVEX_ACTION_SECRET`

## Reference Documentation

- [Stytch Connected Apps CLI](https://stytch.com/docs/guides/connected-apps/cli-app)
- [Stytch JWT Sessions](https://stytch.com/docs/guides/sessions/using-jwts)
- [Convex Custom JWT Auth](https://docs.convex.dev/auth/advanced/custom-jwt)
- [Convex R2 Component](https://www.convex.dev/components/cloudflare-r2)
- [Convex HTTP Actions](https://docs.convex.dev/functions/http-actions)

## Future Features (v2+)

- Web dashboard for session management (view pending/submitted, batch actions, trust rules)
- Yanking (user requests data deletion after submission)
- User pre-review of extracted knowledge
- Granular admin access permissions
- Partial consent/redaction
- Admin dashboard for debugging
- Windows support (if not easy to include in v1)
- Claude Code for web support
