#!/bin/bash
# Stop hook: when the last assistant message is long (>100 words and more than
# one non-blank line), block once and ask for a one-sentence TL;DR. The /focus
# view then renders just that sentence for each long reply.
#
# Fail open: any parse trouble means exit 0 — never a wrong block, never a
# non-zero exit. Requires only bash 3.2 (macOS default), no jq/python.
#
# The stdin JSON is scanned linearly: one IFS split on backslashes (C speed),
# then a state machine over the parts. Word and non-blank-line counts
# accumulate incrementally, so no decoded copy of the message is ever built
# and escape-dense messages stay far under the hook timeout.

input=$(cat) || exit 0

case $input in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

# First sighting of /focus enabled: record it so the SessionStart nudge stops.
if [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ ! -f "$CLAUDE_PLUGIN_DATA/seen-focus" ] \
  && grep -q '"briefTranscript"[[:space:]]*:[[:space:]]*true' "$HOME/.claude.json" 2>/dev/null; then
  mkdir -p "$CLAUDE_PLUGIN_DATA" 2>/dev/null && touch "$CLAUDE_PLUGIN_DATA/seen-focus" 2>/dev/null
fi

# Locate the message key WITHOUT ${var#*pattern} over the whole input: that
# matcher is quadratic in bash 3.2 (measured in whole seconds at tens of KB).
# A fixed-window scan stays linear — the failing case glob is linear, and the
# quadratic %% only ever runs on the one 4KB window that contains the key.
# (An escaped occurrence inside a string value can never match: its quotes
# are preceded by backslashes, so the exact quote-name-quote bytes never
# appear there.)
KEY='"last_assistant_message"'
klen=${#KEY}
ilen=${#input}
off=0
found=-1
while [ "$off" -lt "$ilen" ]; do
  window=${input:$off:4224} # 4096 step + overlap so a straddling key is seen
  case $window in
    *"$KEY"*)
      pre=${window%%"$KEY"*}
      found=$((off + ${#pre}))
      break
      ;;
  esac
  off=$((off + 4096))
done
[ "$found" -ge 0 ] || exit 0
rest=${input:$((found + klen))}
case $rest in
  ':"'*) rest=${rest:2} ;;
  ': "'*) rest=${rest:3} ;;
  *) exit 0 ;; # key present but value shape unexpected: fail open
esac

b='\'
words=0
nonblank=0
line_has=0  # current line has a non-space character
prev_join=0 # last counted character was non-space (a word may continue)

# count_literal <chunk>: fold a run of plain characters (no newlines, no
# backslashes) into the word/line tallies. Word splitting via positional
# params — no herestring (each <<< costs a temp file, ruinous in a hot loop).
count_literal() {
  local chunk=$1 n
  [ -n "$chunk" ] || return 0
  case $chunk in
    *[![:space:]]*) line_has=1 ;;
    *)
      prev_join=0
      return 0
      ;;
  esac
  set -f
  # shellcheck disable=SC2086
  set -- $chunk
  set +f
  n=$#
  case ${chunk:0:1} in
    [[:space:]]) : ;;
    *) [ "$prev_join" = 1 ] && n=$((n - 1)) ;;
  esac
  words=$((words + n))
  case ${chunk:${#chunk}-1:1} in
    [[:space:]]) prev_join=0 ;;
    *) prev_join=1 ;;
  esac
}

count_newline() {
  [ "$line_has" = 1 ] && nonblank=$((nonblank + 1))
  line_has=0
  prev_join=0
}

count_nonspace_char() {
  line_has=1
  [ "$prev_join" = 1 ] || words=$((words + 1))
  prev_join=1
}

# Indexed access into a bash 3.2 array walks a linked list, so iterate with
# for (linear overall) rather than parts[i] (quadratic overall).
IFS="$b" read -ra parts <<<"$rest" || exit 0
closed=""
opener=0 # 1 = this part follows an escape-opening backslash
first=1
for part in "${parts[@]}"; do
  lit=$part
  if [ "$first" = 1 ]; then
    first=0
  else
    if [ "$opener" = 1 ]; then
      if [ -z "$part" ]; then
        # backslash escaping the next backslash: one literal backslash
        count_nonspace_char
        opener=0 # that second backslash was data, not an opener
        continue
      fi
      esc=${part:0:1}
      lit=${part:1}
      case $esc in
        n | r | f) count_newline ;; # line boundaries for splitlines()
        t) prev_join=0 ;;
        b) count_nonspace_char ;;
        u)
          hex=${lit:0:4}
          lit=${lit:4}
          [[ $hex =~ ^[0-9a-fA-F]{4}$ ]] || exit 0 # invalid escape: fail open
          case $hex in
            0009 | 0020) prev_join=0 ;;
            000[aAbBcCdD] | 2028 | 2029) count_newline ;;
            *) count_nonspace_char ;; # other code point: one non-space char
          esac
          ;;
        \" | /) count_nonspace_char ;;
        *) exit 0 ;; # JSON defines no other escapes: malformed, fail open
      esac
    fi
    # opener=0: the preceding backslash was consumed data; part is literal
  fi
  qpre=${lit%%\"*}
  if [ "$qpre" != "$lit" ]; then
    # unescaped quote: end of the message value
    count_literal "$qpre"
    closed=1
    break
  fi
  count_literal "$lit"
  opener=1
done
[ -n "$closed" ] || exit 0 # no terminating quote: malformed, fail open
[ "$line_has" = 1 ] && nonblank=$((nonblank + 1))

if [ "$words" -gt 100 ] && [ "$nonblank" -gt 1 ]; then
  printf '%s\n' '{"decision": "block", "reason": "Please TL;DR your last message in one plain sentence, no \"TL;DR:\" prefix."}'
fi
exit 0
