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

### 3. Session Statistics During Extraction

**Status:** Not started
**Effort:** Medium-Large

Compute mechanical statistics during session extraction to help agents understand session character without semantic analysis:

**Statistics to compute:**
- User message count
- Assistant message count
- Total lines modified (from Edit/Write tools)
- Number of files touched
- **Lines modified by folder** (see algorithm below)
- Bash commands executed count
- Web fetches count
- Web searches count

**Folder breakdown algorithm:**

Find the most specific folder level that captures meaningful distribution. If 90% of work was in `src/frontend/components/` and 10% in `src/backend/`, showing just `src/` loses information.

The exact algorithm and threshold parameters should be determined through experimentation. Initial approach to try:
1. Build a tree of paths with line counts at leaves
2. Traverse depth-first; at each node with >= threshold% of total, check children
3. If any child has >= threshold%, recurse into that child
4. Otherwise, output this node as a "significant" folder
5. Aggregate remaining small items into "other" if needed

This finds the "significant frontier" - the deepest level where folders still represent meaningful chunks of work. The threshold (e.g., 30%) and aggregation rules need tuning based on real session data.

**Acceptance criteria:**
- Statistics computed during extraction and stored in session metadata
- Folder breakdown uses adaptive algorithm to find right granularity
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

**Status:** Not started
**Effort:** Medium-Large

Replace current line-based truncation with word-level truncation that adapts to total output size.

**Core idea:** Longer messages are often less information-dense. Instead of truncating each message to a fixed length, find a uniform truncation length that hits a target total word count.

**Example:**
- Target: 100 words total
- Messages: 20 words, 50 words, 100 words (170 total)
- Algorithm finds truncation length T such that: `min(20,T) + min(50,T) + min(100,T) = 100`
- Result: T = 40, so output is 20 + 40 + 40 = 100 words

**Algorithm:** Find the single uniform truncation length T that achieves the target total. This requires solving for T given the message lengths and target - the exact algorithm needs to be determined.

**Behavior by content type:**
- User/assistant message text: truncate with algorithm above
- Thinking blocks: hidden by default
- Tool results (Read, Edit, etc.): hidden by default

**Show/hide flags:**

Add `--show` and `--hide` flags that accept comma-separated message types with hierarchical field specifiers:

```bash
# Examples
bun cli.js read <id> --show thinking
bun cli.js read <id> --show tool:Bash:input,tool:Bash:output
bun cli.js read <id> --hide user
bun cli.js read <id> --show tool:Read:output --hide tool:Edit:output
```

**Type hierarchy:**
- High level: `user`, `assistant`, `thinking`, `tool`
- Tool level: `tool:Bash`, `tool:Read`, `tool:Edit`, `tool:Write`, etc.
- Field level: `tool:Bash:input`, `tool:Bash:output`, `tool:Read:output`, etc.

More specific selectors override less specific. For example, `--hide tool --show tool:Bash:input` hides all tool content except Bash inputs.

The exact type/field taxonomy should be designed based on the session schema and documented clearly in the CLI help message.

**Acceptance criteria:**
- Word-level truncation replaces line-level
- Single uniform truncation length applied to all messages
- Longer messages naturally get truncated more
- `--show` and `--hide` flags with hierarchical type selectors
- Type hierarchy and available selectors documented in `read --help`
- Works with range reads (see #6)

---

### 6. Range Reads

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
- Can combine with other flags (`--full`, `--show-thinking`, etc.)

---

### 7. Switch to Opus Model

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
4. **#3 Session statistics** - Medium-large, requires extraction changes
5. **#5 Adaptive truncation** - Medium-large, algorithm work needed
6. **#6 Range reads** - Small once #5 is done
7. **#7 Opus model** - Trivial, do after validating improvements
