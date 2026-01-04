# Session 8: JSONL Format Deep Dive

**Date**: 2026-01-04

## Goal

Thoroughly reverse-engineer the Claude Code JSONL transcript format to prepare for session extraction in hive-mind.

## Key Findings

### Entry Types

| Type | Purpose |
|------|---------|
| `summary` | Session descriptions (but beware contamination bug) |
| `user` | User messages and tool results |
| `assistant` | Claude responses |
| `system` | Events (compact_boundary, api_error, stop_hook_summary, local_command) |
| `file-history-snapshot` | File backup tracking for undo |
| `queue-operation` | User input queued while Claude responds |

### Conversation Chain

Messages link via `uuid` → `parentUuid`. Multiple children of same parent = branches (from `/rewind`). Active branch = leaf with most recent timestamp.

### Summary Behavior

**Critical bug discovered**: ~80% of summaries are contaminated from other sessions (GitHub #2597).

- **Intended**: `/resume` creates pointer file with 1 summary; conversation continues in original file
- **Bug**: On startup, Claude Code writes ALL generated summaries to current session file

**Finding correct summary**: Look for summaries where `leafUuid` exists as a `uuid` in the same file.

### Storage Statistics (from preliminary session)

| Metric | Value |
|--------|-------|
| Total raw size | 423 MB (392 sessions) |
| Without base64 | 185 MB (56% reduction) |
| Fully cleaned | 34 MB (92% reduction) |

**Main bloat sources**:
- Base64 images/PDFs: 239 MB (56%)
- File reads stored twice in user messages
- `originalFile` in edit results (83% of edit result size)

## Decisions

### What to Extract for hive-mind

Focus on "cleaned" data:
1. Strip base64 content (images, PDFs)
2. Remove duplicate file contents
3. Keep `structuredPatch`, drop `originalFile`
4. Preserve conversation structure (uuid chains)

### Summary Handling

For now, document the bug and use internal-leafUuid summaries only. Proper fix would require Claude Code update.

## Artifacts Created

- `docs/claude-code-jsonl-format.md` - Comprehensive format reference
- Cloned reference implementations:
  - `simonw/claude-code-transcripts`
  - `jhlee0409/claude-code-history-viewer`

## Resources Referenced

- [GitHub #2597](https://github.com/anthropics/claude-code/issues/2597) - Summary contamination bug
- [GitHub #2272](https://github.com/anthropics/claude-code/issues/2272) - Related discussions
- [Simon's blog post](https://simonwillison.net/2025/Dec/25/claude-code-transcripts/) - Format overview
- [Technical guide](https://idsc2025.substack.com/p/the-complete-technical-guide-to-claude) - Additional context

## Experiments Run

1. **Resume behavior**: Created `/resume` in fresh folder to isolate intended vs bug behavior
2. **Branch detection**: Tested `/rewind` to understand branching structure
3. **Timeline analysis**: Mapped cross-session references to confirm bug pattern
