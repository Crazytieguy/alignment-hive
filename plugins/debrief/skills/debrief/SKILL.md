---
name: debrief
description: This skill should be used when the user asks for a debrief, review or report of what was done in this session, or for another round of an earlier debrief. Also offer it when a long stretch of work the user wasn't watching is finished. Not for reviewing code this session did not write.
---

!`MIN_HIVE_CLI=0.1.23; hive debrief preflight --min "$MIN_HIVE_CLI" --session "${CLAUDE_SESSION_ID}" 2>&1 || printf 'debrief: hive CLI missing, too old, or session stamp unavailable; install or update the CLI (the command is in /hive:align), update the hive plugin, restart\n'`

If preflight failed, resolve it with the user before authoring.

Unless the user asks otherwise, a fork writes the debrief, following `${CLAUDE_PLUGIN_ROOT}/references/authoring.md`; its prompt only says it is the debrief fork for this round, since it already has this conversation. Leave its model unset, so it keeps this session's. Its fact-check subagent (the reference's Flow, step 4) is part of the task, not re-delegation, so the fork spawns it despite its usual instruction to execute directly. If you write the debrief yourself, follow that reference. Debriefs live in this plugin's data directory, `${CLAUDE_PLUGIN_DATA}`, which every `hive debrief dir`, `render` and `capture` call takes as `--data`. If the fork returns before steps 4 and 5 of the reference's Flow, run them yourself.

Publish each round's `page.html` to the first round's artifact (pass its URL), so the reader's seen marks and open sections carry over. On the first round, pass `capabilities: {"db": {"rules": [{"path": "seen", "read": "admin", "write": "owner"}, {"path": "seen/{self}", "read": "interact", "write": "interact"}]}, "user": {}}`, which keeps each reader's seen marks in the artifact database (`seen/<reader id>`); later rounds omit `capabilities` to keep them. Post its link and the "Left for you" lines in chat. Without an Artifact tool, give the user the `page.html` path to open locally instead; seen marks then stay in their browser. Comments that ask for changes to the work are yours to act on, before the next round. Then send the fork all the comments, each with what it was placed on, and the new round number; spawn a fresh fork instead if this session has compacted or much has happened since the last round.
