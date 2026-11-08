#!/bin/bash
# SessionStart hook: tells the model its own session id, which a debrief it writes records as the
# parent `session` (references/authoring.md). Prints nothing unless session_id is a UUID.
#
# Runs on every source. A fork carries the conversation, this hook's earlier line included, into a
# new session id whose transcript holds all of it, so the fork's line marks itself current.
# Compaction and /clear get the line too, in case the earlier one is gone.
set -euo pipefail

jq -ce '
  .session_id?
  | select(type == "string")
  | select(test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"))
  | {hookSpecificOutput: {
      hookEventName: "SessionStart",
      additionalContext: ("Debrief parent session id: " + . + " (current; replaces any earlier line like this)")
    }}
' || true
