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

. "${0%/*}/lib.sh" 2>/dev/null || exit 0

IFS= read -r -d '' input || : # builtin read to EOF; no cat fork per turn

case $input in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

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
step=4096
wlen=$((step + klen - 1)) # overlap so a key straddling a step is still seen
off=0
found=-1
while [ "$off" -lt "$ilen" ]; do
  window=${input:$off:$wlen}
  case $window in
    *"$KEY"*)
      pre=${window%%"$KEY"*}
      found=$((off + ${#pre}))
      break
      ;;
  esac
  off=$((off + step))
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
# backslashes) into the word/line tallies. One word-split via positional
# params does double duty — zero fields means an all-whitespace chunk — with
# no herestring (each <<< costs a temp file, ruinous in a hot loop). The
# caller keeps globbing disabled (set -f) for the whole scan.
count_literal() {
  local chunk=$1 n
  [ -n "$chunk" ] || return 0
  # shellcheck disable=SC2086
  set -- $chunk
  n=$#
  if [ "$n" -eq 0 ]; then
    prev_join=0
    return 0
  fi
  line_has=1
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
# for (linear overall) rather than parts[i] (quadratic overall). Globbing off
# for the whole scan: count_literal word-splits unquoted.
set -f
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
set +f
[ -n "$closed" ] || exit 0 # no terminating quote: malformed, fail open
[ "$line_has" = 1 ] && nonblank=$((nonblank + 1))

if [ "$words" -gt 100 ] && [ "$nonblank" -gt 1 ]; then
  # A TL;DR is being requested while /focus is on: the user is seeing the
  # pairing in action, so retire the SessionStart nudge forever.
  if ! focus_seen && focus_is_on; then
    mark_focus_seen
  fi
  printf '%s\n' '{"decision": "block", "reason": "Please TL;DR your last message in one plain sentence, no \"TL;DR:\" prefix."}'
fi
exit 0
