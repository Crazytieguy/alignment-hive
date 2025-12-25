# Memory System Plan

Living document tracking the design and implementation of the alignment-hive memory system.

## Overview

A system for MATS researchers to contribute session learnings to a shared knowledge base, retrievable by Claude in future sessions.

### Core Principles
- **Storage over retrieval**: Capture as much as possible now; retrieval can improve later
- **Yankability**: Users can retract consent and have their data removed
- **Human review**: All contributed content reviewed before merging
- **Privacy-conscious**: Sensitive data must not leak into the shared repository

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         USER SESSION                                │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼ SessionEnd hook
┌─────────────────────────────────────────────────────────────────────┐
│  Prompt user: "Submit this session to MATS memory?"                 │
│  If approved → submit to processing pipeline                        │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼ Processing (GitHub Action or server)
┌─────────────────────────────────────────────────────────────────────┐
│  Claude inspects session → generates structured content             │
│  Creates PR with structured content (raw data stored separately?)   │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼ Human review
┌─────────────────────────────────────────────────────────────────────┐
│  Reviewer merges PR → marketplace updates → users get new content   │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│                          RETRIEVAL                                  │
│  Subagent invoked by main agent → searches index → returns context  │
└─────────────────────────────────────────────────────────────────────┘
```

## Planned Sessions

Ordered by foundation (earlier sessions inform later ones).

### Session 1: Privacy & Storage Architecture

**Why first**: Determines the fundamental split between what goes in git vs elsewhere. All other decisions depend on this.

**Questions to resolve**:
- Do we store raw JSONL in git, or in a separate server?
- If server: what's the minimum viable server? Who hosts it? How do we authenticate?
- If git-only: how do we handle mistakes that leak sensitive data? (git history is permanent)
- What sanitization is possible/reliable for raw session data?
- Should we skip raw JSONL entirely and only store processed content (like the Sionic blog)?
- Tradeoff: raw data enables reprocessing with improved pipelines, but increases privacy risk

**Outcome**: Decision on storage architecture, documented rationale

### Session 2: Hook Behavior & User Prompting

**Why second**: Need to understand technical capabilities before designing the submission UX.

**Questions to resolve**:
- Does SessionEnd hook trigger reliably? What about on compaction?
- Does the hook have access to session_id and transcript_path?
- Can hooks prompt for user input? If not, what are the alternatives?
  - System notifications
  - Desktop app
  - Stop hook that has Claude ask
  - User-invoked command instead of automatic hook
- Hook timeout constraints (default 60s) - enough for git operations?
- What environment variables/context does the hook receive?

**Experiments to run**:
- Create test SessionEnd hook, observe when it fires
- Test different prompting mechanisms
- Measure time for git clone + branch + push

**Outcome**: Chosen approach for submission UX, documented hook capabilities/limitations

### Session 3: Structured Content Format

**Why third**: Defines what we're actually storing. Informs index design and retrieval.

**Questions to resolve**:
- What metadata for each session? (date, author/pseudonym, project context, labels)
- What structured content? Taking inspiration from Sionic blog:
  - Goal/objective
  - Key insights/learnings
  - Failed attempts (often most valuable)
  - Working configurations/code snippets
  - Lessons learned
- How detailed should descriptions be for retrieval matching?
- Attribution format for yankability (needs to identify author without necessarily exposing name publicly)

**Outcome**: Template/schema for structured session content

### Session 4: Index Design

**Why fourth**: Depends on content format decisions.

**Questions to resolve**:
- Single monolithic index file vs per-session index entries?
- Format: greppable text, JSONL (for jq), or something else?
- What fields in index entries? (id, date, author-id, labels, short description)
- How to handle concurrent PRs updating the index?
  - Option A: PRs don't touch index, regenerate on merge
  - Option B: Each session folder has its own small index file, retrieval searches all of them
- Where does the index live relative to session data?

**Outcome**: Index schema and update strategy

### Session 5: First-Time Setup & Multi-Environment

**Why fifth**: Can design setup flow once we know what needs to be configured.

**Questions to resolve**:
- Where should the alignment-hive clone live? User-configurable path?
- How to handle users with multiple environments (local machine, cloud VM, etc.)?
- What's stored per-environment vs synced across environments?
- First-run experience: what happens when user installs plugin for first time?
- How to make re-setup painless (e.g., new cloud machine)?

**Outcome**: Setup flow design, configuration schema

### Session 6: GitHub Action Processing Pipeline

**Why sixth**: Depends on storage architecture, content format, and index design.

**Questions to resolve**:
- How does Claude Code inspect the session?
  - Read JSONL directly?
  - Use /export for readable transcript?
  - --resume to load as native context? (may not work on different machine)
- How to handle the JSONL format? (schema not well documented)
- Processing pipeline versioning - track which version processed each session
- PR format: what files are created/modified?
- How to enable reviewer feedback → Claude iteration before merge?

**Outcome**: GitHub Action workflow implementation

### Session 7: Retrieval Subagent

**Why seventh**: Depends on index design and content format.

**Questions to resolve**:
- Subagent description: what triggers main agent to invoke it?
- Tools available to subagent: grep, jq, custom search script?
- How much context to return? (full session content vs summary)
- Performance: how fast can it search as index grows?

**Outcome**: Retrieval subagent implementation

### Session 8: Testing Strategy

**Why last**: Need the full system designed before we can test it.

**Questions to resolve**:
- How to test submission flow locally?
- Use existing sessions from this machine as test data?
- Staging branch/repo for integration testing?
- Dry-run mode that shows what would happen without pushing?
- What's the representative project to test with?

**Outcome**: Test plan and initial test run

## Open Questions (Cross-Cutting)

These may come up in multiple sessions:

- **Consent model**: Users consent per-session. How to handle consent changes if we expand access beyond MATS?
- **Rollback procedure**: If sensitive data slips through review, what's the remediation process?
- **Marketplace auto-update**: Verify if plugins auto-update or require manual `/plugin update`
- **Naming**: What should we call the plugin/skill? (memory? knowledge-base? sessions?)

## Design Decisions Log

Record decisions as they're made:

| Date | Decision | Rationale |
|------|----------|-----------|
| 2024-12-24 | Storage prioritized over retrieval | Can improve retrieval later; losing data is permanent |
| 2024-12-24 | Human review required for all submissions | Quality control, privacy protection |
| 2024-12-24 | Attribution required for yankability | Users must be able to identify and remove their data |
| 2024-12-24 | Derive labels from content, not user input | Reduces friction; AI can extract this |

## Session Notes

### Session 0: Initial Planning (2024-12-24)

Established overall architecture and identified key questions. See sections above.

Key insight: Privacy/sanitization is more foundational than originally thought. The decision about whether to store raw JSONL in git (enabling reprocessing but risking leaks) vs a separate server (safer but more infrastructure) affects nearly everything else.

Reference material reviewed:
- [Claude Code Skills Training blog post](https://huggingface.co/blog/sionic-ai/claude-code-skills-training) - Sionic AI's approach with /retrospective and /advise commands
- Existing project-setup plugin structure in this repo
- Claude Code hooks/skills/commands documentation
