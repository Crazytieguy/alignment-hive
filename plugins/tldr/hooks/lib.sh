#!/bin/bash
# Shared focus-state protocol, sourced by both hooks: how /focus state is
# detected and how "the user saw a TL;DR requested while /focus was on" is
# recorded. The policy decision of WHEN to mark lives with the callers
# (the Stop hook only, at the moment it requests a TL;DR).

focus_is_on() {
  grep -q '"briefTranscript"[[:space:]]*:[[:space:]]*true' "$HOME/.claude.json" 2>/dev/null
}

focus_seen() {
  [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ -f "$CLAUDE_PLUGIN_DATA/seen-focus" ]
}

mark_focus_seen() {
  [ -n "${CLAUDE_PLUGIN_DATA:-}" ] || return 0
  mkdir -p "$CLAUDE_PLUGIN_DATA" 2>/dev/null || return 0
  : >"$CLAUDE_PLUGIN_DATA/seen-focus" 2>/dev/null || :
}
