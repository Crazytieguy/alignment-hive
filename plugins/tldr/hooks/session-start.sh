#!/bin/bash
# SessionStart hook: inject the TL;DR instruction every session. Until the
# user has seen a TL;DR requested while /focus was on (forever-sentinel,
# written by the Stop hook only), also show a one-line user-facing nudge —
# skipped while /focus is already on, since there is nothing to point at.
#
# Messages use ONLY textual backslash-u001b escapes (JSON-decoded by Claude
# Code); this file must contain no literal control bytes. Never fails the
# session.

. "${0%/*}/lib.sh" 2>/dev/null || exit 0

U='\'"u001b"
BOLD="${U}[1m"
MAGENTA="${U}[1;35m"
RESET="${U}[0m"

# Model-facing instruction. The \" produce JSON-escaped quotes.
CONTEXT="When the Stop hook asks you to TL;DR your last message, reply with one plain sentence and no \\\"TL;DR:\\\" prefix. Don't shorten or pre-summarize messages to preempt the hook — the separate TL;DR message is what lets /focus toggle between the summary and the full message. If the user asks for more detail — especially detail you've already given — they may be seeing only the TL;DRs; tell them to turn /focus off."

nudge=""
if ! focus_seen && ! focus_is_on; then
  # /focus only exists in the fullscreen renderer; tui may be set in any of the four settings files.
  if grep -qs '"tui"[[:space:]]*:[[:space:]]*"fullscreen"' "$USER_SETTINGS" "${USER_SETTINGS%.json}.local.json" "${CLAUDE_PROJECT_DIR:-.}/.claude/settings.json" "${CLAUDE_PROJECT_DIR:-.}/.claude/settings.local.json"; then
    nudge="${BOLD}tldr:${RESET} run ${MAGENTA}/focus${RESET} to collapse long replies to their TL;DRs"
  else
    nudge="${BOLD}tldr:${RESET} run ${MAGENTA}/tui fullscreen${RESET}, then ${MAGENTA}/focus${RESET} to collapse long replies to their TL;DRs"
  fi
fi

if [ -n "$nudge" ]; then
  printf '{"systemMessage": "%s", "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "%s"}}\n' "$nudge" "$CONTEXT"
else
  printf '{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "%s"}}\n' "$CONTEXT"
fi
exit 0
