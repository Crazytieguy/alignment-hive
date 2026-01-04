# Session 10B: Local Retrieval Plan

CLI tools and agent for searching past sessions. **Depends on 10A** (reads extracted files).

## Overview

Provide tools for searching and browsing extracted session history. Can run concurrently with Sessions 11 and 12 after 10A completes.

## Prerequisites

- Session 10A complete (extracted session files exist)
- Schemas from 10A available in `src/lib/schemas.ts`

## CLI Commands

### `hive-mind index`

Build and display session index from metadata first-lines.

**Output:** Table with session ID, date, title, message count.

**Implementation:**
- Read all files in `.claude/hive-mind/sessions/`
- Parse first line of each (metadata)
- Display as formatted table

### `hive-mind scan <session-id> [--tokens-per-msg=N]`

Scan session with truncation. `--tokens-per-msg` (default: 50) controls average tokens per message output.

**Linear truncation scaling**: Limits scale linearly with `--tokens-per-msg`. Base limits at tokens-per-msg=50:

| Type | Field | Base Limit (chars) |
|------|-------|-------------------|
| **user** | plain text | 200 |
| **user** | tool_result: Bash stdout | 150 |
| **user** | tool_result: Glob/Grep filenames | 100 |
| **user** | tool_result: Edit old/new strings | 80 |
| **user** | tool_result: WebFetch content | 100 |
| **user** | tool_result: Task content | 100 |
| **assistant** | text | 80 |
| **assistant** | thinking | 50 |
| **assistant** | tool_use params | 60 |
| **system** | content | 80 |

At `--tokens-per-msg=100`, limits double. At `--tokens-per-msg=25`, limits halve.

**Fields hidden in scan** (only shown in full read):
- `structuredPatch` from Edit results

**Output format** (cat -n style, single-line per message):
```
     1	[user] 10:00:05 | "Help me implement the login feature with OAuth..."
     2	[assistant] 10:00:12 | [thinking:45c] I'll help... [Read: src/auth.ts]
     3	[user] 10:00:15 | [Read: src/auth.ts → 156 lines]
     4	[user] 10:00:18 | [Bash: npm test → 0] "Tests passed..."
     5	[assistant] 10:00:25 | [Edit: src/auth.ts] old:"func..." new:"async func..."
```

Format details to finalize after testing on real data.

### `hive-mind read <session-id> <indices>`

Get full content for specific messages.
- Parses index ranges: `5`, `5-10`, `1,5,10-15`
- Skips metadata line automatically
- Outputs full JSON entries

## Retrieval Agent

**File:** `plugins/hive-mind/agents/retrieval.md`

Load `/plugin-dev:agent-development` skill before writing.

**Key points for prompt:**
- Use aggressively whenever historical context might help
- Especially valuable in plan mode
- Natural language prompt from main agent describes what to look for
- Workflow: index → filter candidates → scan with appropriate detail → read specific messages
- Output: Concise factual findings with session references
- Detail level guidance: More total messages across sessions → lower tokens-per-msg
  - Example heuristic: `tokens_per_msg = clamp(20, 100, 5000 / total_messages)`

## Implementation Order

1. **CLI: index** (`src/commands/index.ts`)
   - Read metadata first-lines, display table

2. **CLI: scan** (`src/commands/scan.ts`)
   - Linear truncation implementation
   - Output formatting

3. **CLI: read** (`src/commands/read.ts`)
   - Index parsing, full content retrieval

4. **Update cli.ts**
   - Add imports for new commands (separate block from other imports)

5. **Retrieval agent** (`plugins/hive-mind/agents/retrieval.md`)
   - Load skill, write prompt

## Tuning Plan

**Phase 1: Initial calibration**
1. Extract sessions from 3+ projects
2. Measure output at different `--tokens-per-msg` values
3. Verify truncation limits produce readable output

**Phase 2: Format testing**
1. Generate scan output on real sessions
2. Compare readability and token counts
3. Finalize format choices

**Phase 3: Agent testing**
1. Run retrieval agent on test queries
2. Adjust tokens-per-msg heuristic based on retrieval accuracy
3. Iterate on agent prompt

**Target:** ~100k tokens max for full retrieval workflow (index + scans + reads).

## File Structure After Completion

```
hive-mind-cli/
├── src/
│   ├── cli.ts                    # MODIFIED: add index/scan/read commands
│   └── commands/
│       ├── index.ts              # NEW
│       ├── scan.ts               # NEW
│       └── read.ts               # NEW

plugins/hive-mind/
└── agents/
    └── retrieval.md              # NEW
```

## Merge Conflict Avoidance

- Commands in separate files (no conflicts with 11/12)
- `cli.ts` changes: Add commands as a separate import block at the end
- No shared library files with 11/12

## Success Criteria

1. `hive-mind index` shows all extracted sessions
2. `hive-mind scan <id>` produces readable single-line summaries
3. `hive-mind read <id> <indices>` returns full message content
4. Retrieval agent successfully finds relevant past context
5. No merge conflicts with concurrent 11/12 work

## Open Items

1. Scan output format details: Finalize after testing on real data
2. Truncation base limits: Calibrate on real sessions
3. Retrieval agent prompt: Write after loading skill
