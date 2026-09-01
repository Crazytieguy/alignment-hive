#!/bin/bash
# SessionStart hook: inject the TL;DR instruction every session. Until the
# user has been seen with /focus enabled once (forever-sentinel in
# CLAUDE_PLUGIN_DATA), also show a one-line user-facing nudge pointing at it.
#
# Messages use ONLY textual backslash-u001b escapes (JSON-decoded by Claude
# Code); this file must contain no literal control bytes. Never fails the
# session.

U='\'"u001b"
BOLD="${U}[1m"
MAGENTA="${U}[1;35m"
RESET="${U}[0m"

# Model-facing instruction. The \" produce JSON-escaped quotes.
CONTEXT="When the Stop hook asks you to TL;DR your last message, reply with one plain sentence and no \\\"TL;DR:\\\" prefix. Don't shorten or pre-summarize messages to preempt the hook — the separate TL;DR message is what lets /focus toggle between the summary and the full message."

nudge=""
if [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ -f "$CLAUDE_PLUGIN_DATA/seen-focus" ]; then
  : # user already knows /focus
elif grep -q '"briefTranscript"[[:space:]]*:[[:space:]]*true' "$HOME/.claude.json" 2>/dev/null; then
  # /focus is on right now — record it instead of nudging.
  if [ -n "${CLAUDE_PLUGIN_DATA:-}" ]; then
    mkdir -p "$CLAUDE_PLUGIN_DATA" 2>/dev/null && touch "$CLAUDE_PLUGIN_DATA/seen-focus" 2>/dev/null
  fi
elif grep -q '"tui"[[:space:]]*:[[:space:]]*"fullscreen"' "$HOME/.claude/settings.json" 2>/dev/null; then
  nudge="${BOLD}tldr:${RESET} run ${MAGENTA}/focus${RESET} to collapse long replies to their TL;DRs"
else
  # /focus only exists in the fullscreen renderer; best-effort detection.
  nudge="${BOLD}tldr:${RESET} run ${MAGENTA}/tui fullscreen${RESET}, then ${MAGENTA}/focus${RESET} to collapse long replies to their TL;DRs"
fi

if [ -n "$nudge" ]; then
  printf '{"systemMessage": "%s", "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "%s"}}\n' "$nudge" "$CONTEXT"
else
  printf '{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "%s"}}\n' "$CONTEXT"
fi
exit 0
