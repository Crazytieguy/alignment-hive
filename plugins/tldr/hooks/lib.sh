#!/bin/bash
# /focus-state helpers shared by both hooks. Only the Stop hook writes the
# seen-focus sentinel, at the moment it requests a TL;DR while /focus is on.

# CLAUDE_CONFIG_DIR relocates both .claude.json and the user settings.json.
if [ -n "${CLAUDE_CONFIG_DIR:-}" ]; then
  CLAUDE_JSON="$CLAUDE_CONFIG_DIR/.claude.json"
  USER_SETTINGS="$CLAUDE_CONFIG_DIR/settings.json"
else
  CLAUDE_JSON="$HOME/.claude.json"
  USER_SETTINGS="$HOME/.claude/settings.json"
fi

focus_is_on() {
  grep -q '"briefTranscript"[[:space:]]*:[[:space:]]*true' "$CLAUDE_JSON" 2>/dev/null
}

focus_seen() {
  [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ -f "$CLAUDE_PLUGIN_DATA/seen-focus" ]
}

mark_focus_seen() {
  [ -n "${CLAUDE_PLUGIN_DATA:-}" ] || return 0
  mkdir -p "$CLAUDE_PLUGIN_DATA" 2>/dev/null || return 0
  : >"$CLAUDE_PLUGIN_DATA/seen-focus" 2>/dev/null || :
}
