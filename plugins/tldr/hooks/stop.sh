#!/bin/bash
# Stop hook: when the last assistant message is long (>100 words and more than
# one non-blank line), block once and ask for a one-sentence TL;DR. The /focus
# view then renders just that sentence for each long reply.
#
# Fail open: any parse trouble means exit 0 — never a wrong block, never a
# non-zero exit. Requires only bash 3.2 (macOS default), no jq/python.

input=$(cat) || exit 0

case $input in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

# First sighting of /focus enabled: record it so the SessionStart nudge stops.
if [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ ! -f "$CLAUDE_PLUGIN_DATA/seen-focus" ] \
  && grep -q '"briefTranscript"[[:space:]]*:[[:space:]]*true' "$HOME/.claude.json" 2>/dev/null; then
  mkdir -p "$CLAUDE_PLUGIN_DATA" 2>/dev/null && touch "$CLAUDE_PLUGIN_DATA/seen-focus" 2>/dev/null
fi

# Cut everything through the opening quote of the message value.
rest=${input#*'"last_assistant_message":"'}
if [ "$rest" = "$input" ]; then
  rest=${input#*'"last_assistant_message": "'}
  [ "$rest" = "$input" ] && exit 0
fi

# Scan to the terminating unescaped quote, decoding JSON escapes as we go.
# Each iteration consumes one whole literal chunk plus one escape, so the
# loop count is the number of escapes in the message, not its length.
q='"'
b='\'
msg=""
closed=""
while [ -n "$rest" ]; do
  chunk=${rest%%["$q$b"]*}
  msg+=$chunk
  rest=${rest:${#chunk}}
  case $rest in
    "$q"*)
      closed=1
      break
      ;;
    "$b"?*)
      esc=${rest:1:1}
      rest=${rest:2}
      case $esc in
        n | r | f) msg+=$'\n' ;; # \r and \f are line boundaries for splitlines()
        t) msg+=$'\t' ;;
        b) msg+='?' ;;
        u)
          hex=${rest:0:4}
          rest=${rest:4}
          [[ $hex =~ ^[0-9a-fA-F]{4}$ ]] || exit 0 # invalid \u escape: fail open
          case $hex in
            0009) msg+=$'\t' ;;
            000[aAbBcCdD] | 2028 | 2029) msg+=$'\n' ;;
            0020) msg+=' ' ;;
            *) msg+='?' ;; # other code points: one non-space placeholder char
          esac
          ;;
        \" | / | "$b") msg+=$esc ;;
        *) exit 0 ;; # JSON defines no other escapes: malformed, fail open
      esac
      ;;
    *) break ;; # lone trailing backslash: malformed
  esac
done
[ -n "$closed" ] || exit 0

words=$(printf '%s' "$msg" | wc -w) || exit 0
nonblank=$(printf '%s\n' "$msg" | grep -c '[^[:space:]]') || true

if [ "${words:-0}" -gt 100 ] 2>/dev/null && [ "${nonblank:-0}" -gt 1 ] 2>/dev/null; then
  printf '%s\n' '{"decision": "block", "reason": "Please TL;DR your last message in one plain sentence, no \"TL;DR:\" prefix."}'
fi
exit 0
