# Session 10A: Extraction Implementation

Foundation for all downstream work. Creates the extracted session files that retrieval, submission, and audit all read from.

## Overview

Process raw Claude Code sessions into sanitized, compact JSONL files. This session implements the core extraction pipeline without retrieval commands or submission logic.

## Dependencies to Install

```bash
bun add zod
```

Note: We originally planned to use Secretlint, but replaced it with a custom implementation based on gitleaks patterns due to performance issues (Secretlint's profiler blocked Bun's event loop).

## Key Design Decisions

### State Management: Metadata in First Line
Each extracted session file is self-contained:
- Line 1: JSON metadata object (extraction info, summary, message count)
- Lines 2+: Extracted message entries

**Benefits**: Atomic session files, easy multi-machine sync, no git merge conflicts.

### Testing: Bun Built-in
- Use `bun test` (Jest-compatible)
- Files: `*.test.ts` in `src/` alongside source

## Implementation Order

### 1. Schemas (`src/lib/schemas.ts`)

Zod v4 schemas for JSONL parsing with resilience to future Claude Code changes.

**Key design decisions:**
- Use `z.looseObject()` for resilience (allows unknown fields from future Claude Code versions)
- Import via `import { z } from "zod"`
- Validate only fields we use, passthrough others

**Entry types to schema:**
| Entry Type | Action |
|------------|--------|
| `summary` | Keep |
| `user` | Keep (transform tool results) |
| `assistant` | Keep (full content including thinking) |
| `system` | Keep (useful for errors, commands) |
| `file-history-snapshot` | Skip |
| `queue-operation` | Skip |

### 2. Sanitization (`src/lib/sanitize.ts` + `src/lib/secret-rules.ts`)

Custom secret detection based on gitleaks patterns (220 rules ported from https://github.com/gitleaks/gitleaks).

**Implementation:**
- `secret-rules.ts`: Auto-generated file with regex patterns, entropy thresholds, and keywords
- `sanitize.ts`: Detection engine with Shannon entropy filtering and keyword pre-filtering
- Covers: Anthropic, OpenAI, AWS, GitHub, Slack, Azure, and 200+ other services
- Aggressive redaction (false positives acceptable)
- Replace detected secrets with `[REDACTED:<rule-id>]`

**Key details:**
- Entropy filtering rejects low-randomness matches (e.g., `ghp_xxxx...` rejected, real tokens pass)
- Keyword pre-filter for performance (rules with keywords only checked if keyword present)
- Rules without keywords (like AWS access tokens) always run
- Recursive sanitization handles nested objects/arrays

### 3. Extraction (`src/lib/extraction.ts`)

Core extraction logic: JSONL parsing, transformation, sanitization.

**Tool Result Transformations:**

| Tool | Fields to Keep |
|------|----------------|
| **Read** | `filePath`, line count only |
| **Edit** | `filePath`, `oldString`, `newString`, `structuredPatch` |
| **Write** | `filePath`, `content` |
| **Bash** | `command`, `stdout`, `stderr`, `exitCode`, `interrupted` |
| **Glob** | `filenames`, `numFiles`, `truncated` |
| **Grep** | `filenames`, `content`, `numFiles` |
| **WebFetch** | `url`, `prompt`, `content` |
| **WebSearch** | `query`, `results` |
| **Task** | `agentId`, `prompt`, `status`, `content` (subagent response) |

**Content Block Transformations:**

| Block Type | Action |
|------------|--------|
| `text` | Keep full |
| `thinking` | Keep full |
| `tool_use` | Keep (name + parameters needed for context) |
| `tool_result` | Transform per tool type above |
| `image` (base64) | Replace with `{"type":"image","size":123456}` |
| `document` (base64) | Replace with `{"type":"document","media_type":"application/pdf","size":456789}` |

**Metadata Fields:**

**Keep** (valuable for context):
- `uuid`, `parentUuid`, `timestamp`, `sessionId`
- `cwd`, `gitBranch`, `version`
- `message.role`, `message.content`, `message.model`

**Skip** (low value for retrieval):
- `requestId`, `message.id`
- `message.usage`
- `slug`, `userType`

### 4. SessionStart Hook Integration (`src/commands/session-start.ts`)

Add extraction to the existing SessionStart flow (after auth check).

**New behavior to add:**
1. Find raw sessions in `~/.claude/projects/<encoded-cwd>/`
2. For each session file:
   - Read first line of extracted file (if exists) to get metadata
   - Compare `rawMtime` to current file mtime
   - If new or modified: extract -> sanitize -> write
3. Log: "Extracted N new sessions"

**Important:** Structure the code to allow Session 11 to add heartbeat/submission logic in a separate section without merge conflicts.

### 5. Tests

**`src/lib/extraction.test.ts`:**
- Tool result transformations (each tool type)
- Content block transformations (base64 replacement)
- Metadata filtering
- Full entry extraction

**`src/lib/sanitize.test.ts`:**
- Secret detection for each provider type
- Nested object sanitization
- Array sanitization
- Edge cases (empty strings, null values)

## Extracted Session File Format

```jsonl
{"_type":"hive-mind-meta","version":"0.1","sessionId":"abc123","machineId":"uuid-here","extractedAt":"2025-01-04T12:00:00Z","rawMtime":"2025-01-04T10:00:00Z","messageCount":45,"summary":"Session title","rawPath":"~/.claude/projects/-Users-yoav-project/abc123.jsonl"}
{"type":"summary","summary":"Hook behavior testing","leafUuid":"..."}
{"type":"user","uuid":"...","parentUuid":null,"timestamp":"...","message":{"role":"user","content":"..."}}
{"type":"assistant","uuid":"...","parentUuid":"...","timestamp":"...","message":{"role":"assistant","content":[...]}}
...
```

First line (`_type: "hive-mind-meta"`) distinguishes metadata from message entries.

## File Structure After Completion

```
hive-mind-cli/
├── src/
│   ├── cli.ts                    # (unchanged)
│   ├── commands/
│   │   ├── login.ts
│   │   └── session-start.ts      # MODIFIED: add extraction
│   └── lib/
│       ├── auth.ts
│       ├── config.ts             # MODIFIED: add getMachineId
│       ├── messages.ts
│       ├── output.ts
│       ├── extraction.ts         # NEW
│       ├── extraction.test.ts    # NEW
│       ├── sanitize.ts           # NEW
│       ├── sanitize.test.ts      # NEW
│       ├── secret-rules.ts       # NEW (auto-generated from gitleaks)
│       ├── gitleaks.test.ts      # NEW (ported test cases)
│       └── schemas.ts            # NEW

~/.claude/hive-mind/              # Global state
├── auth.json                     # JWT tokens (existing)
└── machine-id                    # Random UUID for anonymous tracking (NEW)

.claude/hive-mind/                # Per-project, created on extraction
└── sessions/
    └── <session-id>.jsonl        # Self-contained: metadata line + extracted messages
```

## Success Criteria

1. Running `bun run session-start` extracts any new/modified sessions
2. Extracted files are smaller than raw files (bloat removed)
3. No secrets in extracted files (Secretlint verified)
4. All tests pass
5. Code structured for Session 11 to add submission without conflicts

## Open Items

1. System message subtypes: Review real examples to decide usefulness
2. Edge cases in JSONL parsing (malformed entries, partial writes)
