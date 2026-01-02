# Session 2: Hook Behavior & User Prompting

**Date**: 2025-12-31

## Goal

Understand hook behavior, determine reliable session tracking, and design user consent flow.

## What We Tested

### Hook Trigger Behavior

Created test hooks (`test-hooks/log-all-events.sh`) to log all hook events.

| Trigger | SessionEnd? | Notes |
|---------|-------------|-------|
| `/exit` | Yes | Clean exit |
| `Ctrl+D` | Yes | Requires double-tap to confirm |
| `kill -15` | Yes | SIGTERM, `reason: "other"` |
| `kill -9` | No | SIGKILL, no hook fires |
| Terminal close | No | No hook fires |
| `/compact` | No | PreCompact fires, then SessionStart (source: compact) |

### Hook Data Fields

**SessionStart:**
```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/session.jsonl",
  "cwd": "/working/dir",
  "hook_event_name": "SessionStart",
  "source": "startup" | "resume" | "compact"
}
```

**SessionEnd:**
```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/session.jsonl",
  "hook_event_name": "SessionEnd",
  "reason": "exit" | "prompt_input_exit" | "other"
}
```

**PreCompact:**
```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/session.jsonl",
  "hook_event_name": "PreCompact",
  "trigger": "manual" | "auto",
  "custom_instructions": ""
}
```

**Stop:**
```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/session.jsonl",
  "hook_event_name": "Stop",
  "stop_hook_active": false,
  "permission_mode": "..."
}
```

### Transcript Structure

Verified by examining actual session files:

- Format: JSONL (one JSON object per line)
- **Append-only**: Compaction adds `summary` entries but does NOT truncate history
- Entry types: `user`, `assistant`, `summary`, `file-history-snapshot`, `system`
- Threading: Each entry has `uuid` and `parentUuid`
- **Session ID persists through compaction** - same session_id before and after

Entry type counts from test sessions:
- Small session (no compaction): only `file-history-snapshot` entries
- Large session (1 compaction): 116 assistant, 80 user, 1 summary
- Our session (manual /compact): 128 assistant, 70 user, 36 summaries

## Key Findings

1. **SessionEnd is unreliable** for crash detection (kill -9, terminal close don't trigger)
2. **Session ID is stable** - doesn't change on compaction or resume
3. **Transcripts preserve full history** - safe to capture at any time
4. **`source` field distinguishes** fresh sessions from resumes/post-compact
5. **Hooks cannot prompt users** - no native yes/no capability

## Design Evolution

### Initial Design (from documentation research)

Started with SessionEnd + Stop hooks + inactivity timeout controlled by Convex.

### Refinements Through Discussion

**Hook simplification:**
- Realized SessionEnd is redundant if using inactivity timeout
- Simplified to Stop hook only for heartbeat
- Added SessionStart for session registration and submission trigger

**Submission timing:**
- Can't autonomously submit after 24h (no daemon running)
- Solution: SessionStart hook checks for ready sessions
- Added 10-min background script delay for final opt-out window

**Project opt-out elimination:**
- Key insight: plugin installed at project scope, not user scope
- Collaborators automatically get access
- No need for per-project opt-out tracking - plugin presence = participation

**State management:**
- Local state is source of truth (session status, auth)
- Convex replicates for stats/debugging
- Session readiness based on file modified time (no Convex query)

**User prompting:**
- Hooks cannot directly prompt users
- SessionStart stdout goes to Claude's context (some pollution acceptable)
- Need to test JSON output options in Session 3

### Plugin State Storage

Researched official patterns:
- `.claude/settings.local.json` - official for local settings
- `.claude/CLAUDE.local.md` - official for local memory
- No official `plugin-name.local.md` pattern found

Decision: Use `~/.claude/alignment-hive/` for user-scoped state (auth, sessions).

## Decisions Made

### Hooks
| Hook | Purpose |
|------|---------|
| Stop | Heartbeat to Convex (session_id, timestamp, line_count) |
| SessionStart | Register session, display pending, submit ready, show reminders |

### Submission Flow
1. Stop hook → Convex heartbeat (upsert)
2. SessionStart → check local files for ready sessions (> 24h old)
3. Display pending sessions (< 24h) for exclusion
4. Launch background script with 10-min delay for final opt-out
5. Background script uploads if not excluded

### State Split
- **Local** (`~/.claude/alignment-hive/`): auth.json, sessions/*.json
- **Convex**: replicated session data for stats

### Convex API
```
POST session/heartbeat  - Upsert session
POST session/upload     - Upload transcript, trigger processing
POST session/exclude    - Record exclusion for stats
```

## Open Questions Identified

1. **Hook output to user vs Claude** - need testing (Session 3)
2. **Slash command behavior** - confirmed always prompts model
3. **CLI tool vs slash commands** - defer to setup session
4. **Session descriptions** - defer to JSONL session
5. **Summary generation** - likely compaction-only, but needs verification

## Test Artifacts

Test hooks were created in `test-hooks/` during this session and deleted after testing completed.
Hook configuration remains in `.claude/settings.local.json`.

## What Changed in Main Plan

- Updated session list (added Hook Output Testing, Ideas Discussion, Plugin Naming, JSONL Deep Dive)
- Added Hooks section with data fields and behavior
- Added State Management section (local vs Convex)
- Added Convex API section
- Updated architecture diagram with refined flow
- Updated technology decisions table
- Plugin installed at project scope (presence = participation)
