# Session 5: Ideas Discussion

## Overview

Discussed architectural changes and new ideas that emerged during time away from the keyboard.

## Key Decisions

### Storage: R2 Instead of Git

**Decision:** Store processed knowledge in R2, not in Git/plugin marketplace.

**Rationale:**
- Git isn't suited for growing knowledge bases
- Pre-signed URLs from Convex → R2 is cleaner for retrieval
- Decouples knowledge updates from plugin updates

### Processing: Fly.io Instead of GitHub Actions

**Decision:** Use Fly.io machines API for processing, not GitHub Actions.

**Rationale:**
- If not touching Git, GitHub Actions is an awkward middleman
- Fly.io machines API gives direct control over job lifecycle
- Spin up per job (cost dominated by Claude usage, latency doesn't matter)
- Convex can track job state directly

### Auth: WorkOS Instead of Stytch

**Decision:** Use WorkOS for authentication.

**Rationale:**
- Has all needed features (device flow, JWT)
- More established
- Officially supported by Convex
- No paid features needed to start

### Hook Simplification: SessionStart Only

**Decision:** Single SessionStart hook handles everything (extraction, heartbeats, submission).

**Rationale:**
- Simpler than Stop + SessionStart
- More reliable: catches everything on next session start, even after crashes
- Must be idempotent (parallel sessions, last-write-wins for state.json)
- Heartbeat uses timestamp from session content, so it doesn't matter when hook runs

### Local-First Priority

**Decision:** Prioritize local extraction and retrieval for v1, defer remote processing.

**Rationale:**
- Immediate value without waiting for remote infrastructure
- Better privacy UX: users review extracted content, not raw JSONL
- Lower stakes for testing extraction quality
- Decouples "start gathering data" from "build processing pipeline"

### Local State Location

**Decision:**
- Global: `~/.claude/hive-mind/` (auth only)
- Per-project: `.claude/hive-mind/` (sessions, index, state)

**Rationale:**
- Sessions are project-specific
- User decides whether to gitignore (may be useful for collaborators or multi-environment)

### Extraction Approach

**Decision:** Deterministic extraction (no AI for v1), producing thinner JSONL with sanitization and bloat removal.

**Details:**
- Extracted JSONL is what gets uploaded (not raw)
- Use library for API key pattern detection
- Processing hierarchy for later: raw extraction → indexing → cross-session insights

### Retrieval Design

**Decision:** Local-only for v1, using jq/scripts to navigate JSONL.

**Details:**
- `sessions/index.md` for navigation (short descriptions)
- Retrieval agent can get trajectory (truncated), then dive into details
- Defer two-tier (local + remote) retrieval design

### Human Review

**Decision:** Admin web app for processing pipeline review (separate v2 design session).

**Purpose:**
- Review index/content updates
- Manage processing jobs and versions
- View statistics
- Browse uploaded sessions

### Convex API Simplification

**Decision:** Heartbeat includes status for all sessions (including excluded). No separate exclude endpoint needed.

**Rationale:** Users exclude sessions by editing state.json directly, so a separate API call isn't reliable. Heartbeat syncs everything.

## Session Structure Updates

Split into v1 and v2:

**v1 Design (Sessions 6-9):**
- First-Time Setup & Multi-Environment
- JSONL Format Deep Dive
- Local Extraction & Retrieval
- Testing Strategy

**v1 Implementation:** Starts after v1 design sessions complete.

**v2 Design:**
- Processing Pipeline (Fly.io) - for indexing and cross-session insights
- Admin Web App (separate session)

## Open Items Noted

- Opt-out UX: users can edit state.json directly (or ask Claude)
- Graceful degradation: local extraction works even if Convex is down
- JSONL reverse-engineering: reference https://github.com/simonw/claude-code-transcripts

## What's Next

Session 6: First-Time Setup & Multi-Environment
- WorkOS device flow implementation
- CLI tool vs slash commands for user actions
- Multi-environment handling
