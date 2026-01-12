# Retrieval Skill Improvements PRD

This document tracks planned improvements to the hive-mind retrieval skill.

## Overview

The retrieval skill helps agents search historical Claude Code sessions to find past decisions, discussions, and implementations. These improvements aim to make retrieval more effective while keeping the design simple and letting agent intelligence handle semantics.

## Design Principles

- **Minimal prompting** - Let tooling do the heavy lifting; agents are intelligent and will find usage patterns we don't anticipate
- **No semantic processing** - The agent is responsible for understanding meaning; we provide mechanical statistics and search
- **Scope discipline** - This is a memory retrieval agent, not a general research agent

---

## Planned Changes

### 1. Retrieval Metaphor in Prompt

**Status:** Done
**Effort:** Small

Replace procedural instructions with a metaphor that conveys the role. The agent is like a "memory archaeologist" - carefully excavating layers of project history, brushing away noise to uncover the artifacts (decisions, discussions, realizations) that explain why things are the way they are.

The prompt should paint this picture without prescribing specific patterns. Let the agent's intelligence adapt to each query.

**Acceptance criteria:**
- Prompt uses evocative metaphor instead of step-by-step instructions
- Conveys that the job is retrieval of historical facts, not analysis or interpretation
- Keeps total prompt length minimal

---

### 2. Pre-populate Dynamic Context

**Status:** Done
**Effort:** Medium

Include dynamic bash outputs in the skill content so agents start with consistent baseline context:

1. **Full git log** - `$(git log --oneline)` - all commits, not truncated
2. **Session index** - `$(bun ${CLAUDE_PLUGIN_ROOT}/cli.js index)`
3. **CLI help text** - Help output for each subcommand (index, grep, read)

This eliminates the first few commands every agent runs and ensures nothing is missed.

**Acceptance criteria:**
- Skill dynamically includes git log, index, and help text
- Help text shown for each subcommand, not just top-level CLI
- Content updates each time skill is invoked

---

### 3. Session Statistics in Index

**Status:** Done
**Effort:** Medium-Large

Compute mechanical statistics on-the-fly during index to help agents understand session character without semantic analysis.

**Statistics computed:**
- Message counts (total and user)
- Lines added/removed (from Edit/Write tools)
- Files touched
- Significant locations (paths where >30% of work happened)
- Tool usage counts (Bash, WebFetch, WebSearch)

**Implementation notes:**
- Statistics computed on-the-fly during index, not stored in metadata
- Recursively includes stats from subagent sessions
- Two-threshold algorithm for significant locations (>30% of total, >50% of parent)
- Relative paths for project files, ~/ for home directory
- Zero values display as blank

**Acceptance criteria:**
- ~~Statistics computed during extraction and stored in session metadata~~ Computed on-the-fly
- ~~Folder breakdown uses adaptive algorithm to find right granularity~~ Two-threshold algorithm implemented
- Statistics displayed in index output
- Be inclusive initially; we can remove noisy stats later

---

### 4. Add Read-Only Tools to Agent

**Status:** Done
**Effort:** Small

Add Claude's built-in read-only tools to the agent's tools field:
- `Read`
- `Glob`
- `Grep`

The prompt will continue to only mention bash commands. The additional tools are available if the agent needs them for cross-referencing, but we don't actively encourage their use.

**Acceptance criteria:**
- Agent has access to Read, Glob, Grep tools
- Prompt does not mention these tools
- `allowed-tools` restriction removed (since we're explicitly listing tools)

---

### 5. Adaptive Word-Level Truncation

**Status:** Done
**Effort:** Medium-Large

Replace current line-based truncation with word-level truncation that adapts to total output size.

**Core idea:** Longer messages are often less information-dense. Instead of truncating each message to a fixed length, find a uniform truncation length that hits a target total word count.

**Example:**
- Target: 100 words total
- Messages: 20 words, 50 words, 100 words (170 total)
- Algorithm finds truncation length T such that: `min(20,T) + min(50,T) + min(100,T) = 100`
- Result: T = 40, so output is 20 + 40 + 40 = 100 words

**Algorithm:** Sort messages by length ascending, then iterate to find where the uniform limit fits. For messages shorter than the limit, include them in full; for longer messages, truncate to exactly L words. The limit is clamped to a minimum of 6 words to ensure useful content.

**Implementation:**
- `truncation.ts` - Core utilities: `countWords`, `truncateWords`, `computeUniformLimit`
- `format.ts` - Applies truncation via `formatContentBody` helper
- Default target: 2000 words total
- `--skip N` flag for pagination (skip first N words per field)
- Shows `[Limited to N words per field. Use --skip N for more.]` when truncation applied

**Acceptance criteria:**
- ~~Word-level truncation replaces line-level~~ Done
- ~~Single uniform truncation length applied to all messages~~ Done
- ~~Longer messages naturally get truncated more~~ Done
- ~~Default behavior: show user/assistant text, hide thinking and tool results~~ Done (thinking shows word count only)
- Works with field filtering (#6) and range reads (#7)

---

### 6. Field Filtering and Output Control

**Status:** Not started
**Effort:** Medium

Add consistent field filtering to both `read` and `grep` commands. The API should feel familiar to users of tools like grep, jq, and xsv.

**API design considerations:**

Before implementation, research and document how similar tools handle field selection:
- `grep` - basic pattern matching, `-o` for match-only, context flags
- `jq` - path expressions (`.foo.bar`), filtering, projection
- `xsv` - `select` subcommand with column names/indices
- `rg` (ripgrep) - `--type`, `--glob` for file filtering
- `miller` - field selection with `-f`

The goal is an API that's intuitive for users familiar with these tools, not something novel.

**Capabilities needed:**

For `read`:
- Select which message types to show/hide (user, assistant, thinking, tool results)
- Drill into tool-specific fields (e.g., show Bash inputs but not outputs)
- Hierarchical: more specific selectors override less specific

For `grep`:
- Select which fields to search over (currently searches all text?)
- Same show/hide controls for output formatting as `read`
- Field selection should use same syntax as `read` for consistency

**Possible syntax directions:**

```bash
# jq-style paths
bun cli.js read <id> --show .assistant --hide .tool.Read.output

# grep-style types
bun cli.js grep <pattern> --type user,assistant --field input

# Colon-separated hierarchy (current PRD direction)
bun cli.js read <id> --show tool:Bash:input --hide tool:Edit
```

The exact syntax should be determined during implementation based on what works best for session data structure.

**Acceptance criteria:**
- `read` supports show/hide for message types and tool fields
- `grep` supports field selection for search scope
- `grep` supports same output filtering as `read`
- Syntax is consistent between commands
- API documented in help text with examples
- Design rationale documented (which CLI tools influenced the design)

---

### 7. Range Reads

**Status:** Not started
**Effort:** Small (after #5)

Add support for reading a range of entries:

```bash
bun cli.js read <session-id> 50-100
```

Reads entries 50 through 100, applying the adaptive truncation from #5.

**Acceptance criteria:**
- `read <id> N-M` syntax works
- Truncation adapts to total range size
- Can combine with other flags (`--full`, field filtering from #6, etc.)

---

### 8. Switch to Opus Model

**Status:** Planned for later
**Effort:** Trivial

Add model override to skill frontmatter:

```yaml
model: opus
```

For most users within subscription limits, the improved reasoning is worth the cost. Implement after other improvements are validated.

**Acceptance criteria:**
- Skill specifies opus model
- Validate retrieval quality improvement justifies cost

---

## Not Planned

These were considered but deferred or rejected:

- **Semantic signals** (topics, decision density) - Agent handles semantics
- **Key moment extraction** - Too complex for now
- **Grep snippets** - Already implemented (`-C N` flag)
- **Semantic search with embeddings** - Future consideration
- **Decision tracking as first-class objects** - Future consideration
- **Cross-session linking** - Future consideration

---

## Implementation Order

Suggested sequence based on dependencies and effort:

1. ~~**#4 Add read-only tools** - Small, unblocks other testing~~ Done
2. ~~**#1 Retrieval metaphor** - Small, improves baseline behavior~~ Done
3. ~~**#2 Pre-populate context** - Medium, significant UX improvement~~ Done
4. ~~**#3 Session statistics** - Medium-large, computed on-the-fly in index~~ Done
5. ~~**#5 Adaptive truncation** - Medium-large, algorithm work needed~~ Done
6. **#6 Field filtering** - Medium, API design then implementation
7. **#7 Range reads** - Small once #5 is done
8. **#8 Opus model** - Trivial, do after validating improvements
