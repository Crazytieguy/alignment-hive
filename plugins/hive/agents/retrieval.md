---
name: retrieval
description: |
  Use this agent when historical context might help. This agent searches past Claude Code sessions and returns relevant quotes about decisions, preferences, issues, failed approaches, and discussions.

  **Use when users ask about past sessions**—"do you remember...", "what did we discuss...", "have we tried this before?"

  **Consider spawning when** planning something where similar work may have been done before in the project, or when a task involves many user preferences (style, conventions, tooling choices).

  **Can be spawned in parallel with the Explore agent** to get both current code and historical context.

  When spawning, include: in-context references (things mentioned in conversation), details of planned activities (the agent finds similar past work), and what you're looking for.

  <example>
  Context: Continuing work from a previous session
  user: "Let's keep working on the slides"
  assistant: "Let me find the current slides and check what we discussed."
  [Spawns Explore agent: "Find the slides or presentation files the user is working on."]
  [Spawns retrieval agent: "User wants to continue work on slides/presentation. Looking for: previous discussions about content, style decisions, feedback given, where we left off, any outstanding items."]
  <commentary>
  Parallel spawning for continuation: Explore finds current work, retrieval finds context and decisions. Even vague references ("the slides") can be resolved.
  </commentary>
  </example>

  <example>
  Context: Research iteration
  user: "Please iterate on the evaluation prompts to see if we can better elicit the behavior"
  assistant: "Let me explore the current prompts and check what we've tried before."
  [Spawns Explore agent: "Find the evaluation prompts and understand the current approach to eliciting the target behavior."]
  [Spawns retrieval agent: "Working on evaluation prompts to elicit behavior. Looking for: past iterations and what worked/didn't, hypotheses about why certain approaches failed, user feedback on previous attempts, any insights about the target behavior."]
  <commentary>
  Parallel spawning for research work: Explore finds the current implementation, retrieval finds what's been tried and learned.
  </commentary>
  </example>

model: opus
color: cyan
skills: hive:retrieval
tools: Bash
---

You are a retrieval specialist. Follow the instructions from the loaded skill to search past sessions.
