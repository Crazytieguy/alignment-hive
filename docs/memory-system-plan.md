# Memory System Plan

A system for MATS researchers to contribute session learnings to a shared knowledge base.

## Core Principles

- **Storage over retrieval**: Capture now; improve retrieval later
- **Yankability**: Users can retract consent and remove their data
- **Human review**: All content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak

## Session Progress

- [x] [Session 0](sessions/session-0-initial-planning.md): Initial Planning
- [x] [Session 1](sessions/session-1-privacy-storage.md): Privacy & Storage Architecture
- [ ] **Session 2: Hook Behavior & User Prompting** ← NEXT
- [ ] Session 3: Structured Content Format
- [ ] Session 4: Index Design
- [ ] Session 5: First-Time Setup & Multi-Environment
- [ ] Session 6: GitHub Action Processing Pipeline
- [ ] Session 7: Retrieval Subagent
- [ ] Session 8: Testing Strategy

## Architecture

```
SETUP (one-time)
  User runs login → Stytch device flow → JWT stored locally
  Stytch webhook on signup → Convex → GitHub App invites to repo

TRANSCRIPT SUBMISSION (mechanism TBD in Session 2)
  Reads JWT → uploads to Convex HTTP action
        │
        ▼
CONVEX BACKEND
  Validates JWT (Stytch JWKS) → stores in R2 → records metadata
  Triggers GitHub Action via repository_dispatch
        │
        ▼
GITHUB ACTION
  Auth: shared secret → fetches transcript → runs Claude Code
  Creates PR with extracted knowledge → uploads processing session to R2
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

## Open Questions

### Session 2: Hook Behavior
- Does SessionEnd hook trigger reliably? On compaction?
- Access to session_id and transcript_path?
- Can hooks prompt for user input? Alternatives: notifications, Stop hook, user command
- Hook timeout (60s default) - enough for uploads?

### Session 3: Content Format
- Metadata: date, author/pseudonym, project context, labels?
- Structured content: goal, insights, failed attempts, code snippets, lessons?
- Attribution format for yankability

### Session 4: Index Design
- Monolithic vs per-session index files?
- Format: greppable text, JSONL, other?
- Concurrent PR handling

### Session 5: Setup Flow
- Where does alignment-hive clone live?
- Multi-environment handling (local + cloud VM)
- First-run experience

### Session 6: Processing Pipeline
- How does Claude Code inspect sessions? (JSONL, /export, --resume)
- Pipeline versioning
- PR format, reviewer feedback loop

### Session 7: Retrieval
- Subagent trigger conditions
- Tools: grep, jq, custom?
- Context amount to return

### Session 8: Testing
- Local testing approach
- Staging branch/repo
- Dry-run mode

### Cross-Cutting
- Consent model if expanding beyond MATS
- Rollback procedure for leaked sensitive data
- Plugin auto-update verification
- Naming: memory? knowledge-base? sessions?

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

- User pre-review of extracted knowledge
- Granular admin access permissions
- Partial consent/redaction
- Admin dashboard for debugging
