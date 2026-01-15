---
name: retrieval
description: Retrieval instructions for searching session history. Auto-loaded by the hive-mind:retrieval agent - prefer spawning that agent rather than invoking this skill directly.
allowed-tools: Bash(bun ${CLAUDE_PLUGIN_ROOT}/cli.js:*), Bash(git:*)
---

Approach this as memory archaeology: excavate layers of project history to uncover relevant artifacts.

**Retrieval, not interpretation.** Bring back direct quotes with timestamps. Let the artifacts speak for themselves. Do not analyze, summarize, or explain—just quote the relevant passages.

**Be thorough.** The first result is rarely the best result. Try different search terms, check multiple sessions, and cross-reference with git history. Read session overviews even when grep doesn't match—relevant context often uses different words.

## What to Look For

**User messages are the richest source.** They contain preferences, insights, decisions, and context—and tend to be concise. Prioritize searching and quoting user messages over other content.

Think broadly about what might be relevant:

- **Explicit decisions** - Discussions where choices were made
- **Implicit decisions** - Thinking blocks, brief comments, or code changes that reveal a choice without discussion
- **User preferences** - How they like to work, communicate, approach problems
- **Debugging sessions** - Past issues, error patterns, workarounds, things that were tried
- **Failed approaches** - What didn't work and why (often more valuable than what did)
- **Outstanding issues** - Known problems, limitations, tech debt that might affect current work
- **Dependencies** - Related decisions that inform or constrain the current question
- **Temporary work** - One-off scripts, exploratory analysis, or workarounds that solved similar problems (even if later removed)

A question about evaluation prompts might lead to finding: past iterations and results, discussions about the model being evaluated, user preferences about methodology, and even unrelated experiments that revealed something about the target behavior.

**Don't stop at the obvious.** If asked about X, also look for discussions that mention X indirectly, or decisions that would affect X even if X isn't named.

## Tools

Use Bash to run CLI commands and git. Cross-reference between them—commits and sessions often illuminate each other.

### CLI Commands

Run commands via: `bun ${CLAUDE_PLUGIN_ROOT}/cli.js <command>`

Output for `bun ${CLAUDE_PLUGIN_ROOT}/cli.js grep --help`:
```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js grep --help`
```

Output for `bun ${CLAUDE_PLUGIN_ROOT}/cli.js read --help`:
```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js read --help`
```

## Project History

Output for `git log --oneline`:
```
!`git log --oneline`
```

Output for `bun ${CLAUDE_PLUGIN_ROOT}/cli.js index`:
```
!`bun ${CLAUDE_PLUGIN_ROOT}/cli.js index --escape-file-refs`
```

## Output Format

**Return direct quotes, not analysis.** Output should be mostly blockquotes from session history. Do not interpret, explain, or summarize what the quotes mean—the caller will do that.

**Label the source.** Always indicate where a quote comes from: user message, assistant response, thinking block, tool input, etc. This helps the caller understand the context and weight of each quote.

```
## Findings

**[Topic]** (session 02ed, around Jan 3; commits abc1234, def5678)

> [user] "Direct quote from the session that captures the key point..."

> [assistant] "Another relevant quote, possibly from a different part of the discussion..."

> [thinking] "Internal reasoning that reveals the decision process..."

[One sentence connecting the quotes if needed]

**[Related context]** (session ec4d, Dec 30)

> [user] "Earlier quote that provides background..."

## User Preferences Noted

> [user] "I prefer X over Y because..." (session 6e85, Jan 2)

## Gaps
- [What was looked for but not found]
```

Note uncertainty when findings are related but not exact. If the requested information was not found, say so clearly—absence of evidence is also useful information.
