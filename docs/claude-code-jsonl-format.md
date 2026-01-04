# Claude Code JSONL Format Reference

Comprehensive documentation of the Claude Code session transcript format, based on reverse-engineering and analysis.

## File Locations

| Location | Purpose |
|----------|---------|
| `~/.claude/projects/<encoded-path>/<session-id>.jsonl` | Session transcripts |
| `~/.claude/projects/<encoded-path>/agent-<id>.jsonl` | Subagent transcripts |
| `~/.claude/history.jsonl` | User input history (for up-arrow, not conversation content) |
| `~/.claude/settings.json` | User settings |

**Path encoding**: Forward slashes replaced with hyphens (e.g., `/Users/yoav/projects/foo` → `-Users-yoav-projects-foo`)

**Retention**: Sessions older than 30 days are automatically deleted unless `cleanupPeriodDays` is configured.

## JSONL Structure

Each line is a complete JSON object. Lines are appended chronologically. The file format is designed to be forward-compatible with optional fields.

## Entry Types

The `type` field determines the entry type:

| Type | Purpose | Location in File |
|------|---------|------------------|
| `summary` | Session descriptions | Usually start of file, but can appear elsewhere |
| `user` | User messages and tool results | Throughout |
| `assistant` | Claude's responses | Throughout |
| `system` | System events (errors, hooks, compaction, commands) | Throughout |
| `file-history-snapshot` | File backup tracking for undo | Throughout |
| `queue-operation` | User input queued while Claude is responding | Throughout |

---

## Summary Entries

```json
{
  "type": "summary",
  "summary": "Human-readable session description",
  "leafUuid": "uuid-of-message-when-summary-was-created"
}
```

| Field | Description |
|-------|-------------|
| `summary` | Text description of the session |
| `leafUuid` | References the message UUID that was the conversation tip when summary was generated |

### Summary Behavior

**When written**: Summaries are generated when a session exits (observable by preceding "Goodbye!" local command output).

**Location**: Usually at the start of the file, but can appear in clusters throughout the file (e.g., after session resume/exit cycles).

**Multiple summaries**: A file may contain multiple summary entries with different `leafUuid` values, representing the evolving session description over time.

### Session Resume Behavior (Intended Design)

When using `/resume` to continue a previous session:

1. **New "pointer" file created**: Contains only a summary entry + file-history-snapshots, with NO actual conversation messages
2. **Original file extended**: The resumed conversation is appended to the original session file
3. **`leafUuid` cross-reference**: The summary in the new file points to the exit message in the original file (the point where the session was exited before resume)

This is an **intended** cross-file reference - the new file serves as a bookmark/pointer to where the conversation continues in the original file.

### Cross-Session Summary Contamination (Known Bug)

As of January 2025, `/resume` appears to be the only intended behavior that creates cross-file `leafUuid` references. Other cross-file references are likely bug contamination.

There is a known bug ([#2597](https://github.com/anthropics/claude-code/issues/2597), as of v2.0.76) where summaries appear in the wrong session file:

1. On startup, Claude Code scans all session files in the project directory
2. Generates summaries for conversation chains that lack them
3. **Bug**: Writes ALL generated summaries to the current session file instead of their original files

**Prevalence**: In one analyzed dataset (v2.0.76), ~80% of summary entries were affected (184/228 summaries with cross-file `leafUuid` references that weren't from `/resume`).

**Symptoms**:
- `leafUuid` pointing to message UUIDs in unrelated session files
- "Stub" files containing only summaries and file-history-snapshots, with no actual messages
- Multiple unrelated sessions sharing the same inherited summaries

### Distinguishing Intended vs Bug Behavior

| Behavior | Source | Characteristics |
|----------|--------|-----------------|
| **Intended** (resume) | `/resume` command | New file has 1 summary pointing to exit message of related session; original file contains the continued conversation |
| **Bug** (contamination) | Startup summary generation | Multiple unrelated summaries; `leafUuid` points to messages in completely unrelated sessions |

### Finding the Correct Summary for a Session

To identify legitimate summaries:

1. **For regular sessions**: Find summaries where `leafUuid` exists as a `uuid` within the same file
2. **For resume pointer files**: The file will have only 1 summary + file-history-snapshots, no messages; the `leafUuid` points to the original session being resumed
3. **Contaminated summaries**: Multiple summaries pointing to various unrelated sessions are likely bug contamination

---

## User Messages

```json
{
  "type": "user",
  "uuid": "unique-message-id",
  "parentUuid": "previous-message-uuid",
  "sessionId": "session-uuid",
  "timestamp": "2025-12-24T10:00:00.000Z",
  "cwd": "/current/working/directory",
  "gitBranch": "main",
  "version": "2.0.76",
  "userType": "external",
  "slug": "session-name-slug",
  "message": {
    "role": "user",
    "content": "string or array"
  }
}
```

### User Message Fields

| Field | Type | Description |
|-------|------|-------------|
| `uuid` | string | This message's unique identifier |
| `parentUuid` | string\|null | Previous message's UUID (null for first message after compaction) |
| `sessionId` | string | Session identifier |
| `timestamp` | string | ISO 8601 timestamp |
| `cwd` | string | Working directory |
| `gitBranch` | string | Git branch name |
| `version` | string | Claude Code version |
| `userType` | string | Always "external" for user messages |
| `slug` | string | Human-readable session name |
| `message.role` | string | Always "user" |
| `message.content` | string\|array | Message content (see Content Types) |

### User Message Flags

| Field | Type | Description |
|-------|------|-------------|
| `isMeta` | boolean | Metadata-only message, not sent to Claude (e.g., local command caveats) |
| `isCompactSummary` | boolean | Continuation summary after compaction |
| `isVisibleInTranscriptOnly` | boolean | Stored but not sent to API |
| `isSidechain` | boolean | Part of a subagent conversation |
| `agentId` | string | Subagent identifier (if from Task tool) |

### User Message - Tool Results

When a user message contains tool results:

| Field | Description |
|-------|-------------|
| `toolUseResult` | Structured result from tool execution |
| `sourceToolUseID` | Links to the `tool_use` that triggered this result |
| `todos` | Todo list state snapshot |
| `thinkingMetadata` | `{level, disabled, triggers}` for thinking configuration |
| `imagePasteIds` | References to pasted images |

---

## Assistant Messages

```json
{
  "type": "assistant",
  "uuid": "unique-message-id",
  "parentUuid": "previous-message-uuid",
  "sessionId": "session-uuid",
  "timestamp": "2025-12-24T10:00:05.000Z",
  "requestId": "req_xxxxx",
  "message": {
    "id": "msg_xxxxx",
    "model": "claude-sonnet-4-20250514",
    "role": "assistant",
    "type": "message",
    "content": [],
    "stop_reason": "end_turn",
    "stop_sequence": null,
    "usage": {}
  }
}
```

### Assistant Message Fields

| Field | Type | Description |
|-------|------|-------------|
| `requestId` | string | API request identifier |
| `message.id` | string | Anthropic message ID |
| `message.model` | string | Model used for response |
| `message.stop_reason` | string | "end_turn", "tool_use", "max_tokens", or "stop_sequence" |
| `message.usage` | object | Token usage statistics |
| `isApiErrorMessage` | boolean | Response is an API error |
| `agentId` | string | Subagent identifier (if subagent response) |
| `isSidechain` | boolean | Part of a subagent conversation |
| `durationMs` | number | Response latency in milliseconds |

### Usage Object

```json
{
  "input_tokens": 1000,
  "output_tokens": 500,
  "cache_creation_input_tokens": 200,
  "cache_read_input_tokens": 800,
  "cache_creation": {
    "ephemeral_5m_input_tokens": 200,
    "ephemeral_1h_input_tokens": 0
  },
  "service_tier": "standard",
  "server_tool_use": {
    "web_fetch_requests": 1,
    "web_search_requests": 0
  }
}
```

**Note**: Multiple consecutive `assistant` entries with the same parent indicate streaming (partial responses being appended).

---

## Content Types

The `message.content` field can be a string or an array of content blocks:

### Text Content
```json
{"type": "text", "text": "Response text here"}
```

### Thinking Content
```json
{
  "type": "thinking",
  "thinking": "Extended reasoning here...",
  "signature": "base64-signature"
}
```

### Tool Use
```json
{
  "type": "tool_use",
  "id": "toolu_01ABC...",
  "name": "ToolName",
  "input": {}
}
```

### Tool Result (in message.content array)
```json
{
  "type": "tool_result",
  "tool_use_id": "toolu_01ABC...",
  "content": "result string or array",
  "is_error": false
}
```

### Image Content
```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "base64-encoded-data"
  }
}
```

### Document Content (PDFs)
```json
{
  "type": "document",
  "source": {
    "type": "base64",
    "media_type": "application/pdf",
    "data": "base64-encoded-data"
  }
}
```

---

## Tool Use Result Structure

The `toolUseResult` field on user messages contains structured results from tool execution.

### Common Fields

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | "completed", "error", etc. |
| `durationMs` | number | Execution time |
| `interrupted` | boolean | If execution was interrupted |
| `truncated` | boolean | If result was truncated |

### Read Tool Result

```json
{
  "file": {
    "filePath": "/path/to/file",
    "content": "file contents...",
    "numLines": 100,
    "startLine": 1,
    "totalLines": 100
  },
  "isImage": false
}
```

**Note**: File content appears in BOTH `toolUseResult.file.content` AND `message.content[].content` (duplication).

### Edit Tool Result

```json
{
  "filePath": "/path/to/file",
  "oldString": "original text",
  "newString": "replacement text",
  "originalFile": "entire file content before edit",
  "structuredPatch": [
    {
      "oldStart": 10,
      "oldLines": 3,
      "newStart": 10,
      "newLines": 5,
      "lines": [" context", "-removed", "+added", " context"]
    }
  ],
  "userModified": false,
  "replaceAll": false
}
```

**Note**: `originalFile` stores the entire file content, which is redundant with `structuredPatch`.

### Write Tool Result

```json
{
  "filePath": "/path/to/file",
  "content": "file contents written"
}
```

### Bash Tool Result

```json
{
  "command": "ls -la",
  "stdout": "output...",
  "stderr": "errors...",
  "exitCode": 0,
  "durationMs": 150,
  "interrupted": false
}
```

### Glob Tool Result

```json
{
  "filenames": ["file1.ts", "file2.ts"],
  "numFiles": 2,
  "truncated": false
}
```

### Grep Tool Result

```json
{
  "filenames": ["file1.ts", "file2.ts"],
  "content": "matching lines...",
  "numFiles": 2,
  "numLines": 10
}
```

### Task (Agent) Tool Result

```json
{
  "agentId": "a051da9",
  "prompt": "original prompt",
  "content": [{"type": "text", "text": "agent response"}],
  "totalDurationMs": 58979,
  "totalTokens": 63383,
  "totalToolUseCount": 18,
  "usage": {},
  "status": "completed"
}
```

### WebSearch Tool Result

```json
{
  "query": "search query",
  "results": [
    {
      "tool_use_id": "toolu_xxx",
      "content": [{"title": "Result", "url": "https://..."}]
    }
  ]
}
```

### WebFetch Tool Result

```json
{
  "url": "https://example.com",
  "prompt": "extraction prompt",
  "content": [{"type": "text", "text": "extracted content"}]
}
```

### TodoWrite Tool Result

```json
{
  "newTodos": [
    {"content": "Task 1", "status": "pending", "activeForm": "Working on Task 1"}
  ],
  "oldTodos": []
}
```

### AskUserQuestion Tool Result

```json
{
  "questions": [
    {
      "question": "Which option?",
      "header": "Choice",
      "options": [{"label": "A", "description": "Option A"}],
      "multiSelect": false
    }
  ],
  "answers": {
    "Which option?": "A"
  }
}
```

---

## System Entries

System entries record various events with different `subtype` values:

### Compact Boundary

Marks where conversation compaction occurred:

```json
{
  "type": "system",
  "subtype": "compact_boundary",
  "content": "Conversation compacted",
  "parentUuid": null,
  "logicalParentUuid": "uuid-of-last-message-before-compaction",
  "compactMetadata": {
    "trigger": "manual",
    "preTokens": 46742
  },
  "timestamp": "2025-12-31T13:51:58.281Z",
  "uuid": "unique-id",
  "level": "info",
  "isMeta": false
}
```

| Field | Description |
|-------|-------------|
| `logicalParentUuid` | References message before compaction (for logical continuity) |
| `compactMetadata.trigger` | "manual" or "auto" |
| `compactMetadata.preTokens` | Token count before compaction |

**After compact_boundary**: The next message is typically a user message with `isCompactSummary: true` containing the continuation summary.

### API Error

```json
{
  "type": "system",
  "subtype": "api_error",
  "level": "error",
  "cause": {
    "code": "FailedToOpenSocket",
    "path": "https://api.anthropic.com/v1/messages",
    "errno": 0
  },
  "error": {},
  "retryInMs": 505.26,
  "retryAttempt": 1,
  "maxRetries": 10,
  "timestamp": "2025-12-31T15:02:56.597Z"
}
```

### Stop Hook Summary

Records when stop hooks executed:

```json
{
  "type": "system",
  "subtype": "stop_hook_summary",
  "hookCount": 1,
  "hookInfos": [{"command": "/path/to/hook.sh"}],
  "hookErrors": [],
  "preventedContinuation": false,
  "stopReason": "",
  "hasOutput": false,
  "level": "suggestion",
  "toolUseID": "uuid"
}
```

### Local Command

Records slash command execution:

```json
{
  "type": "system",
  "subtype": "local_command",
  "content": "<command-name>/config</command-name>\n<command-message>config</command-message>",
  "level": "info",
  "isMeta": false
}
```

---

## File History Snapshot

Tracks file backups for undo functionality:

```json
{
  "type": "file-history-snapshot",
  "messageId": "uuid-of-associated-message",
  "snapshot": {
    "messageId": "uuid",
    "timestamp": "2025-12-30T21:42:41.294Z",
    "trackedFileBackups": {
      "path/to/file.ts": {
        "backupFileName": "7cc3765d13b1ef53@v1",
        "version": 1,
        "backupTime": "2025-12-30T21:48:30.639Z"
      }
    }
  },
  "isSnapshotUpdate": false
}
```

---

## Queue Operation

Records user input queued while Claude is responding:

```json
{
  "type": "queue-operation",
  "operation": "enqueue",
  "content": "queued message text",
  "sessionId": "session-uuid",
  "timestamp": "2025-12-24T14:49:58.701Z"
}
```

| Operation | Description |
|-----------|-------------|
| `enqueue` | Message added to queue |
| `dequeue` | Message removed from queue |
| `remove` | Message removed (e.g., cancelled) |
| `popAll` | Queue cleared (all messages processed) |

---

## Conversation Branching

Branching occurs when users rewind (`/rewind`) or retry messages.

### Structure

- Multiple messages can share the same `parentUuid`
- Each branch continues with its own chain of `uuid` → `parentUuid` links
- All branches are stored chronologically in the file
- No explicit "active branch" marker

### Identifying Branches

```
Message A (uuid: aaa)
    ├── Message B1 (parentUuid: aaa, uuid: bbb1)  ← Branch 1
    │       └── Message C1 (parentUuid: bbb1)
    └── Message B2 (parentUuid: aaa, uuid: bbb2)  ← Branch 2 (rewind)
            └── Message C2 (parentUuid: bbb2)
```

### Finding the Active Branch

1. Build a tree from `parentUuid` relationships
2. Find all "leaves" (messages with no children)
3. The leaf with the most recent `timestamp` is typically the active branch endpoint
4. Trace backwards via `parentUuid` to reconstruct the active conversation

### Leaves

A "leaf" is a message whose `uuid` is not referenced as any other message's `parentUuid`. Sessions commonly have multiple leaves representing abandoned branches.

---

## Subagent Sessions

Files named `agent-<id>.jsonl` contain subagent conversations (from Task tool).

### Characteristics

- All messages have `isSidechain: true`
- All messages have `agentId` matching the filename
- Structure otherwise identical to main sessions
- Results are reported back to main session via `toolUseResult` on a user message

---

## Storage Statistics (Example)

Based on analysis of 392 sessions over a 30-day window on one machine (December 2025):

### Overall Size

| Metric | Size |
|--------|------|
| Total raw | 423 MB |
| Without base64 | 185 MB (56% reduction) |
| Without base64 + file reads + originalFile | 34 MB (92% reduction) |

### Base64 Content Breakdown

| Type | Items | Size |
|------|------:|-----:|
| PDFs | 92 | 114 MB (48%) |
| PNGs | 78 | 104 MB (44%) |
| JPEGs | 48 | 21 MB (8%) |
| **Total** | **218** | **239 MB** |

### Message Type Distribution (Raw)

| Type | Count | Size | % |
|------|------:|-----:|--:|
| user | 5,240 | 408.6 MB | 96.5% |
| assistant | 8,500 | 14.3 MB | 3.4% |
| file-history-snapshot | 1,166 | 0.5 MB | 0.1% |

### User Message Subtypes

| Subtype | Size | % |
|---------|-----:|--:|
| Tool result with base64 file | 254 MB | 62% |
| Document with base64 (PDFs) | 57 MB | 14% |
| Tool result with text file | 34 MB | 8% |
| Tool result (other) | 34 MB | 8% |
| Images with text | 25 MB | 6% |
| Plain text messages | 2 MB | <1% |

### Tool Result (Other) Breakdown

| Category | Size |
|----------|-----:|
| Edit results | 22.6 MB (67%) |
| WebFetch | 2.9 MB |
| Bash | 2.3 MB |
| Glob/Grep | 1.6 MB |
| WebSearch | 1.5 MB |
| Task/Agent | 1.3 MB |

**Edit results store `originalFile`** which is 83% of their size (18.8 MB of 22.6 MB). The `structuredPatch` field contains the same information in diff format.

### Cleaned Data (No base64, file content, or originalFile)

**By content type:**

| Category | Size | % |
|----------|-----:|--:|
| Assistant responses | 15.1 MB | 45% |
| User text messages | 3.3 MB | 10% |
| Edit results (patch only) | 3.0 MB | 9% |
| WebFetch results | 3.0 MB | 9% |
| Bash results | 2.4 MB | 7% |
| Task/Agent results | 2.3 MB | 7% |
| Glob/Grep results | 1.6 MB | 5% |
| WebSearch results | 1.5 MB | 5% |

**Assistant response breakdown:**

| Component | Size | % |
|-----------|-----:|--:|
| Text responses | 1.47 MB | 31% |
| Thinking blocks | 1.29 MB | 27% |
| Edit tool inputs | 0.77 MB | 16% |
| Write tool inputs | 0.51 MB | 11% |
| Bash tool inputs | 0.34 MB | 7% |

### Session Size Distribution (Cleaned)

| Bucket | Sessions | Total Size |
|--------|:--------:|----------:|
| <5 KB | 201 | 0.4 MB |
| 5-20 KB | 51 | 0.5 MB |
| 20-50 KB | 39 | 1.1 MB |
| 50-100 KB | 30 | 1.9 MB |
| 100-250 KB | 34 | 5.4 MB |
| 250-500 KB | 17 | 6.4 MB |
| 500KB-1MB | 15 | 11.9 MB |
| >1 MB | 5 | 6.0 MB |

- Largest cleaned session: 1.37 MB (~351K tokens)
- Median cleaned session: 4.5 KB (~1.2K tokens)

### Key Insights

1. **Base64 dominates storage** - 56% of raw size is base64 (images + PDFs)
2. **Edit results are wasteful** - `originalFile` adds 83% bloat; `structuredPatch` alone suffices
3. **File reads are stored twice** - In `toolUseResult.file.content` AND `message.content[].content`
4. **Sessions auto-cleanup** - Files older than ~30 days are purged
5. **Token estimation** - ~4 chars per token for JSON; cleaned sessions use ~8.8M tokens total

---

## Field Reference by Entry Type

### All Entry Types

| Field | Types | Description |
|-------|-------|-------------|
| `type` | all | Entry type identifier |
| `uuid` | user, assistant, system | Unique message ID |
| `parentUuid` | user, assistant, system | Previous message ID |
| `sessionId` | user, assistant, queue-operation | Session identifier |
| `timestamp` | user, assistant, system, queue-operation | ISO 8601 timestamp |

### User/Assistant Shared

| Field | Description |
|-------|-------------|
| `cwd` | Current working directory |
| `gitBranch` | Git branch name |
| `version` | Claude Code version |
| `userType` | Always "external" |
| `slug` | Session name slug |
| `isSidechain` | Subagent conversation flag |
| `agentId` | Subagent identifier |
| `message` | Message content object |

### User-Specific

| Field | Description |
|-------|-------------|
| `isMeta` | Metadata-only, not sent to Claude |
| `isCompactSummary` | Continuation summary after compaction |
| `isVisibleInTranscriptOnly` | Stored but not sent to API |
| `toolUseResult` | Tool execution result |
| `sourceToolUseID` | Triggering tool_use ID |
| `todos` | Todo list state |
| `thinkingMetadata` | Thinking configuration |
| `imagePasteIds` | Pasted image references |

### Assistant-Specific

| Field | Description |
|-------|-------------|
| `requestId` | API request ID |
| `isApiErrorMessage` | API error indicator |
| `durationMs` | Response latency |
| `message.id` | Anthropic message ID |
| `message.model` | Model used |
| `message.stop_reason` | Why generation stopped |
| `message.usage` | Token statistics |

---

## References

- [claude-code-transcripts](https://github.com/simonw/claude-code-transcripts) - Simon Willison's transcript viewer
- [claude-code-history-viewer](https://github.com/jhlee0409/claude-code-history-viewer) - Tauri-based viewer
- [Simon's blog post](https://simonwillison.net/2025/Dec/25/claude-code-transcripts/) - Format overview
