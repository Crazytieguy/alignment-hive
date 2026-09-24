#!/bin/bash
set -euo pipefail

jq -ce '
  .session_id?
  | select(type == "string")
  | select(test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"))
  | {hookSpecificOutput: {
      hookEventName: "SessionStart",
      additionalContext: ("Debrief parent session id: " + .)
    }}
' || true
