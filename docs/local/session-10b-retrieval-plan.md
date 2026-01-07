# Local Retrieval - Three Session Plan

## Overview

Break retrieval into three incremental sessions:
1. **10B-1**: `index` and `read` (no truncation) - establish format
2. **10B-2**: Add smart truncation to `read`
3. **10B-3**: Implement and optimize retrieval subagent

## Design Principles

- **Token-efficient for LLMs**, not human-readable
- Simple output: no padding, no decorations, no repeated field names
- Use existing schemas from `cli/lib/schemas.ts` (add fields as needed)
- `read` without truncation is the testing ground for format

## CLI Invocation

The CLI is invoked via Bun. During development:
```bash
bun hive-mind/cli/cli.ts <command>
```

In production (after `bun run cli:build`):
```bash
bun ~/.claude/plugins/hive-mind/cli.js <command>
# Or via CLAUDE_PLUGIN_ROOT:
bun $CLAUDE_PLUGIN_ROOT/cli.js <command>
```

## Existing Code Reference

**`hive-mind/cli/lib/extraction.ts`:**
- `readExtractedMeta(path)` - reads metadata from first line of extracted file
- `getHiveMindSessionsDir(cwd)` - returns `.claude/hive-mind/sessions/` path
- `parseJsonl(content)` - generator yielding parsed JSON objects

**`hive-mind/cli/lib/schemas.ts`:**
- `HiveMindMetaSchema` - metadata schema (sessionId, checkoutId, extractedAt, rawMtime, messageCount, summary, rawPath, agentId, parentSessionId)
- `parseKnownEntry()` - parses entry with `KnownEntrySchema`
- Entry types: `SummaryEntrySchema`, `UserEntrySchema`, `AssistantEntrySchema`, `SystemEntrySchema`
- Content blocks: `TextBlockSchema`, `ThinkingBlockSchema`, `ToolUseBlockSchema`, `ToolResultBlockSchema`, `ImageBlockSchema`, `DocumentBlockSchema`
- Uses `z.looseObject()` so unknown fields pass through - add fields to schemas as needed

**`hive-mind/cli/lib/output.ts`:**
- `printError()`, `printSuccess()`, `printInfo()`, `printWarning()`
- `colors.red()`, `colors.green()`, etc.

**`hive-mind/cli/cli.ts`:**
- Simple COMMANDS registry pattern, each command is `{ description, handler }`

---

# Session 10B-1: Index and Read (No Truncation)

## Goal

Establish the output format for session data. `read` shows full content (no truncation, summarization, or redaction) - this is what the subagent sees when requesting a single line in full. Only exclude truly useless fields.

## Files to Create/Modify

| File | Action |
|------|--------|
| `hive-mind/cli/commands/index.ts` | CREATE |
| `hive-mind/cli/commands/read.ts` | CREATE |
| `hive-mind/cli/lib/format.ts` | CREATE |
| `hive-mind/cli/cli.ts` | MODIFY - add commands |

## 1. Index Command

**Usage:** `bun hive-mind/cli/cli.ts index`

List extracted sessions. No field name prefixes - position-based, token-efficient.

Agent sessions grouped under their parent, indented:

```
02ed589a-8b41-4004 2026-01-02T10:15:00 156 OAuth login flow implementation
  agent-a127579 2026-01-02T10:20:00 45 Web search for docs
  agent-b234680 2026-01-02T10:25:00 23 Code review
10d8ce01-2aea-4b20 2026-01-04T14:30:00 57 Refactoring session plan
```

Format: `<truncated-id> <datetime> <msg-count> <summary>`
- Truncate ID to first 16 chars (enough for uniqueness + prefix matching)
- ISO datetime or compact format like `2026-01-02T10:15`
- Agent sessions indented under parent (no need to write parent ID)
- Summary from metadata (may be empty)

## 2. Read Command

**Usage:** `bun hive-mind/cli/cli.ts read <session-id> [indices]`

- No indices = all entries (excluding metadata line)
- Indices: `5`, `5-10`, `1,5,10-15` (1-indexed, after metadata)
- Prefix match on session ID (e.g., `02ed` matches `02ed589a-...`)

**Output:** Full formatted entries. No truncation. Exclude only truly useless fields (like internal IDs that have no retrieval value).

## 3. Format Module (`cli/lib/format.ts`)

The key design work. Format each entry type for token-efficient LLM consumption.

### Entry Format (Draft - Iterate on Real Data)

Header line with metadata, then content. Use XML tags for structured content (multi-field, multi-line). Keeps things simple, efficient, and familiar - not too unique a format.

**User entry with plain text:**
```
1|user|2026-01-02T10:00:05
Help me implement the login feature with OAuth
```

**User entry with tool_result (content blocks):**
```
3|user|2026-01-02T10:00:15
<tool_result name="Read" tool_use_id="xyz123">
  <file_path>/src/auth.ts</file_path>
  <content>
    import { OAuth } from 'oauth';
    export function authenticate() {
      // ... full file content, no truncation ...
    }
  </content>
</tool_result>
```

**Assistant entry with multiple content blocks:**
```
2|assistant|2026-01-02T10:00:12|model:claude-sonnet-4-20250514
<thinking>
  I'll start by reading the auth file to understand the current implementation...
</thinking>
<text>
  Let me read the authentication code first.
</text>
<tool_use name="Read" id="abc123">
  <file_path>/src/auth.ts</file_path>
</tool_use>
<tool_use name="Grep" id="def456">
  <pattern>OAuth</pattern>
  <path>/src</path>
</tool_use>
```

**System entry:**
```
4|system|2026-01-02T10:00:20|subtype:error|level:warn
Permission denied accessing /etc/passwd
```

**Summary entry:**
```
5|summary
OAuth implementation complete. Added login endpoint with Google OAuth provider.
```

### Fields to Include by Entry Type

**user:**
- Line number, type, timestamp
- uuid (useful for conversation threading reference)
- message.content - full content, string or blocks
- cwd, gitBranch (context for where work happened)
- Exclude: parentUuid (can derive from order), sessionId (redundant), version

**assistant:**
- Line number, type, timestamp
- uuid
- model (keep for now; 10B-2 may conditionally drop if repeating)
- message.content - all blocks: text, thinking, tool_use
- stop_reason (if not "end_turn" - indicates interruption/error)
- Exclude: parentUuid, sessionId

**system:**
- Line number, type, timestamp (if present)
- subtype, level, content

**summary:**
- Line number, type
- summary text
- Exclude: leafUuid (internal validation)

### Format Decisions (Subject to Experimentation)

1. **Header separator**: `|` for metadata, newline before content
2. **Datetime**: ISO format `2026-01-02T10:00:05` (precise, unambiguous)
3. **Structured content**: XML tags for multi-field/multi-line blocks
4. **Tool results**: Full content - file contents, command output, everything
5. **Thinking**: Full text (truncation comes in 10B-2)

## 4. CLI Integration

Add to `cli/cli.ts`:
```typescript
import { index } from "./commands/index";
import { read } from "./commands/read";

const COMMANDS = {
  // ... existing ...
  index: { description: "List extracted sessions", handler: index },
  read: { description: "Read session entries", handler: read },
} as const;
```

## Testing

1. Run `bun hive-mind/cli/cli.ts extract` to ensure extracted sessions exist
2. Test `index` on real data, verify format is scannable
3. Test `read <id>` for full session, verify all content visible
4. Test `read <id> 1,5,10` for specific lines
5. Implement snapshot tests to capture format decisions
6. Iterate on format based on what's useful vs noise

---

# Session 10B-2: Smart Redaction

## Goal

Add smart redaction to `read` command for token-efficient scanning. Tool-specific handling for optimal compression.

## Behavior (Implemented)

- `read <id>` (all lines) → redacted (scanning mode)
- `read <id> 5` (single line) → full content
- `read <id> 1-10` (multiple lines) → full content
- `read <id> --full` → force full content for all lines

## Compact Format (Implemented)

Pipe-delimited format for scanning mode. Tool results merged with tool calls.

### Line Format

```
line_num|timestamp|type|content...
```

**Timestamp handling:**
- First entry: full ISO `2026-01-05T02:50`
- Subsequent: just time `02:51`
- New day: include date again

**Entry types:**
- `user|"first line" +Nlines`
- `thinking|Nlines`
- `assistant|"first line" +Nlines`
- `tool|ToolName|...tool-specific...|returned=...`
- `summary|"summary text"`
- `parent=start` for restart points

### Tool-Specific Optimizations

| Tool | Format |
|------|--------|
| **Edit** | `Edit\|path\|-X+Y` (diff stats, no result) |
| **Read** | `Read\|path\|returned=Xlines` |
| **Write** | `Write\|path\|written=Xlines` |
| **Bash** | `Bash\|command="..."\|description="..."\|returned="first line" +Xlines` |
| **Grep** | `Grep\|pattern="..."\|path\|returned=Xlines` |
| **Glob** | `Glob\|pattern="..."\|returned=Xfiles` |
| **Task** | `Task\|agent_session="..."\|description="..."\|prompt=Xlines\|returned=Xlines` |
| **TodoWrite** | `TodoWrite\|todos=N` |
| **AskUserQuestion** | `AskUserQuestion\|questions=N\|returned="answer summary..."` |
| **ExitPlanMode** | `ExitPlanMode\|returned="first line..."` |
| **Generic** | `ToolName\|key="value"\|...\|returned=Xlines` |

### Example Output

```
1|2026-01-05T02:50|user|"I'd like to iterate a bit on @hive-mind-cli/src/lib/schem..." +3lines
2|02:51|thinking|45lines
3|02:51|assistant|"Let me analyze these questions carefully." +36lines
4|02:51|tool|TodoWrite|todos=4
5|02:51|thinking|1line
6|02:51|tool|Edit|/Users/yoav/projects/alignment-hive/hive-mind-cli/src/lib/schemas.ts|-36+57
...
18|02:52|user|parent=start|"I'd like to iterate a bit on @hive-mind-cli/src/lib/schem..." +3lines
...
21|02:53|tool|Task|agent_session="agent-aacc414"|description="Explore ToolResultBlock recursion"|prompt=10lines|returned=99lines
...
95|03:21|tool|Bash|command="bun test"|description="Run hive-mind-cli tests"|returned="bun test v1.2.14 (6a363a38)" +62lines
```

### Deferred Optimizations

- **Path shortening**: Could shorten `/Users/username/projects/alignment-hive/` to relative paths
- **Model dedup**: Remove `model` when same as previous message
- **MCP tools**: Generic format works, but could add custom formatters for common MCP tools

## Summaries Investigation Notes

From 10B-1 investigation:
- Agent sessions never have summaries (too short to trigger compaction)
- Some main sessions lack summaries (session ended before compaction)
- Summaries have `leafUuid` referencing the last message of summarized branch
- **Index**: Agent sessions excluded (no summaries)
- **Fallback**: Sessions without summaries show first line of first user prompt

## Deferred Ideas

### Smart Truncation (If Simple Redaction Isn't Enough)

Content-type-specific limits with linear scaling based on lines requested:
- 1 line: no truncation
- 10 lines: moderate truncation
- 100+ lines: heavy truncation

**Base limits (at moderate truncation, ~10 lines) - calibrate on real data:**

| Content Type | Base Limit (chars) |
|--------------|-------------------|
| user plain text | 200 |
| user tool_result: Bash stdout | 150 |
| user tool_result: Glob/Grep filenames | 100 |
| user tool_result: Edit old/new strings | 80 |
| user tool_result: Read file content | 100 |
| user tool_result: WebFetch content | 100 |
| user tool_result: Task output | 100 |
| assistant text | 80 |
| assistant thinking | 50 |
| assistant tool_use params | 60 |
| system content | 80 |

**Scaling formula:** `limit = base * scale_factor(lines_requested)`
- Example: `scale_factor = clamp(0.5, 5.0, 10 / lines_requested)`

**Fields to conditionally drop/hide:**
- `structuredPatch` from Edit results (large, redundant with old/new)
- Repeating fields if same as previous entry: model, cwd, gitBranch
- Full file contents beyond limit

### Git Commit Correlation

Consider correlating git commit timestamps with Claude Code sessions to find related commits even if they weren't made by Claude. This could help with:
- Finding commits that were made manually after Claude's work
- Identifying sessions related to specific code changes
- Building a timeline of session + commit activity

---

# Session 10B-3: Retrieval Subagent

## Goal

Create retrieval agent that searches past sessions for relevant context. Use `/plugin-dev:agent-development` skill before writing.

## File

`plugins/hive-mind/agents/retrieval.md`

## Agent Frontmatter (Rough Draft - Subject to Change After Loading Skill)

```yaml
---
name: retrieval
description: Search past Claude Code sessions for relevant context. Use when historical decisions, implementations, or discussions might inform the current task. Especially valuable during planning.
tools:
  - Bash(bun $CLAUDE_PLUGIN_ROOT/cli.js index:*)
  - Bash(bun $CLAUDE_PLUGIN_ROOT/cli.js read:*)
  - Bash(grep:*)
  - Bash(head:*)
  - Bash(tail:*)
---
```

Tools rationale:
- `index` and `read` for session access
- `grep` for filtering/piping output (main use case: grep hive-mind command output)
- `head`/`tail` for output management

## Agent Description (Critical)

The description must reliably cause the main agent to provide the right amount of context when spawning. The agent can't retrieve usefully if it doesn't know what it's looking for.

Key elements to iterate on:
- Clear trigger conditions (when to use)
- Guidance on what context to include in the prompt

## Agent Workflow

1. **Index sessions**: `bun $CLAUDE_PLUGIN_ROOT/cli.js index` to see available sessions
2. **Identify candidates**: Based on timestamps and summaries
3. **Read all lines first**: Always start with `read <id>` (all lines, heavily truncated) to get overview
4. **Drill into interesting entries**: Request specific line ranges or single lines for full content
5. **Grep for keywords**: Pipe output through grep to filter

**Workflow rationale:**
- Always start with truncated overview of entire session
- Truncation level is automatic based on lines requested
- Single line = full content, many lines = truncated overview
- Drill down progressively to find relevant details

## Agent Prompt Content (Rough Draft)

- When to use: planning, debugging, understanding history, finding patterns
- Output format: concise factual findings with session/line references
- Token budget guidance: ~100k tokens max for full retrieval workflow
- Example queries and workflows
- Emphasize: overview first, then drill down

## Plugin Registration

Update `plugins/hive-mind/plugin.json` to register the agents directory.

---

# Implementation Checklist

## 10B-1: Index and Read
- [x] Create `cli/lib/format.ts` with entry formatting functions
- [x] Create `cli/commands/index.ts`
- [x] Create `cli/commands/read.ts`
- [x] Update `cli/cli.ts` with new commands
- [x] Test on real extracted sessions
- [x] Implement snapshot tests
- [x] **Experiment and iterate**: Subagent reviews led to cleaner format (removed cwd/branch, tool IDs, text wrappers)

## 10B-2: Smart Redaction
- [x] Add `redactMultiline()` function to `format.ts` (MVP)
- [x] Update `formatSession()` with `{ redact: boolean }` option
- [x] Update `read` command: default redacted, `--full` for full content
- [x] Single line requests get full content automatically
- [x] Update snapshot tests with redacted versions (all 8 sessions)
- [x] Tool-specific redaction (Edit, Read, Write, Bash, Grep, Glob, Task, TodoWrite, AskUserQuestion, etc.)
- [x] Timestamp compression (full first, then HH:MM, new day gets date)
- [x] `parent="start"` for restart points (was `parent="?"`)
- [x] **Format change**: Compact pipe-delimited format for redacted mode
- [ ] Path shortening (deferred)
- [ ] Model dedup (deferred)
- [ ] MCP tool-specific formatters (deferred)

## 10B-3: Retrieval Subagent
- [ ] Load `/plugin-dev:agent-development` skill
- [ ] Create `plugins/hive-mind/agents/retrieval.md`
- [ ] Update `plugins/hive-mind/plugin.json`
- [ ] Test agent on sample queries
- [ ] **Experiment and iterate**: Refine description to get good context from main agent, adjust prompt based on results
