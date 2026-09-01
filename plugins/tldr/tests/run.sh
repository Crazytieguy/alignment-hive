#!/bin/bash
# Dev-only test harness for the tldr hooks. Not executed by the plugin at
# runtime; uses python3 to build JSON inputs and assert on JSON outputs.
# The hooks under test stay pure bash. Run: bash plugins/tldr/tests/run.sh
#
# No literal backslash-u sequences or control bytes appear in this file (the
# Write tool and the agent sanitizer both rewrite them); escapes are built at
# runtime from pieces.
set -euo pipefail
cd "$(dirname "$0")"
STOP=../hooks/stop.sh
START=../hooks/session-start.sh

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
pass=0
fail=0

b='\'
BU="${b}u" # textual backslash-u, for crafting encoded JSON by hand

# build_stop_input <python-expr-for-text> [extra-json-object-entries]
build_stop_input() {
  python3 - "$1" "${2:-}" <<'EOF'
import json, sys
text = eval(sys.argv[1])
obj = {"session_id": "s", "transcript_path": "/tmp/t.jsonl", "hook_event_name": "Stop",
       "stop_hook_active": False, "last_assistant_message": text}
extra = sys.argv[2]
out = json.dumps(obj)
if extra:
    out = out[:-1] + ", " + extra + "}"
print(out)
EOF
}

# run_stop <home> <plugin_data or "UNSET"> — stdin passes through to the hook
run_stop() {
  if [ "$2" = "UNSET" ]; then
    env -u CLAUDE_PLUGIN_DATA HOME="$1" bash "$STOP"
  else
    env CLAUDE_PLUGIN_DATA="$2" HOME="$1" bash "$STOP"
  fi
}

check() { # <name> <actual> <expected>
  if [ "$2" = "$3" ]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    printf 'FAIL %s\n  expected: %q\n  actual:   %q\n' "$1" "$3" "$2"
  fi
}

BLOCK='{"decision": "block", "reason": "Please TL;DR your last message in one plain sentence, no \"TL;DR:\" prefix."}'
assert_block() { check "$1" "$2" "$BLOCK"; }
assert_silent() { check "$1" "$2" ""; }

H="$TMP/home"
D="$TMP/data"
mkdir -p "$H"

long_multiline="' '.join(f'w{i}' for i in range(60)) + chr(10)*2 + ' '.join(f'v{i}' for i in range(60))"
long_oneline="' '.join(f'w{i}' for i in range(120))"
short_msg="'just a short reply'"

# --- Stop hook: block/no-block classification ---
out=$(build_stop_input "$short_msg" | run_stop "$H" "$D")
assert_silent "short message" "$out"

out=$(build_stop_input "$long_multiline" | run_stop "$H" "$D")
assert_block "long multi-line blocks" "$out"

out=$(build_stop_input "$long_oneline" | run_stop "$H" "$D")
assert_silent "long single-line passes" "$out"

out=$(build_stop_input "$long_multiline" | sed 's/"stop_hook_active": false/"stop_hook_active": true/' | run_stop "$H" "$D")
assert_silent "stop_hook_active guard" "$out"

out=$(build_stop_input "$long_oneline + ' literal ' + chr(92) + 'n text'" | run_stop "$H" "$D")
assert_silent "literal backslash-n text is not a newline" "$out"

out=$(build_stop_input "$long_oneline + chr(10)" | run_stop "$H" "$D")
assert_silent "trailing newline only is one non-blank line" "$out"

out=$(build_stop_input "$long_oneline + chr(10) + '   ' + chr(10) + '  '" | run_stop "$H" "$D")
assert_silent "blank-only extra lines don't count" "$out"

out=$(build_stop_input "'quoted ' + chr(34) + 'words' + chr(34) + ' and back' + chr(92) + 'slashes' + chr(10) + $long_multiline" | run_stop "$H" "$D")
assert_block "escaped quotes/backslashes decode" "$out"

out=$(build_stop_input "$short_msg" "\"z_trailing\": \"$(python3 -c "print('pad '*200, end='')")\"" | run_stop "$H" "$D")
assert_silent "fields after the message are ignored" "$out"

out=$(printf '%s' '{"stop_hook_active": false, "last_assistant_message":"no closing quote here' | run_stop "$H" "$D")
assert_silent "unterminated string fails open" "$out"

out=$(printf '%s' '{"stop_hook_active": false}' | run_stop "$H" "$D")
assert_silent "missing message field fails open" "$out"

# --- Stop hook: hand-crafted encoded escapes ---
W101=$(python3 -c "print('w '*101, end='')")
NL="${BU}000a"

raw='{"stop_hook_active": false, "last_assistant_message":"'"${W101}${NL}${W101}"'"}'
out=$(printf '%s' "$raw" | run_stop "$H" "$D")
assert_block "encoded u000a acts as newline" "$out"

# Words joined by encoded u0020 must count as separate words: 60 + 60 words
# over two lines blocks; a placeholder would collapse line 2 into one word.
sep=$(python3 -c "print((chr(92)+'u0020').join('v%d' % i for i in range(60)), end='')")
raw='{"stop_hook_active": false, "last_assistant_message":"'"$(python3 -c "print('w '*60, end='')")${NL}${sep}"'"}'
out=$(printf '%s' "$raw" | run_stop "$H" "$D")
assert_block "encoded u0020 separates words" "$out"

raw='{"stop_hook_active": false, "last_assistant_message":"'"${W101}${NL}${b}q more words"'"}'
out=$(printf '%s' "$raw" | run_stop "$H" "$D")
assert_silent "invalid escape fails open" "$out"

raw='{"stop_hook_active": false, "last_assistant_message":"'"${W101}${NL}${BU}zzzz tail"'"}'
out=$(printf '%s' "$raw" | run_stop "$H" "$D")
assert_silent "invalid u-escape hex fails open" "$out"

# Deliberate semantics: a validly terminated message string is classified even
# if the JSON after it is malformed — the value was fully extracted.
raw='{"last_assistant_message":"'"${W101}${NL}${W101}"'", broken}'
out=$(printf '%s' "$raw" | run_stop "$H" "$D")
assert_block "garbage after a valid close still classifies" "$out"

# Performance regression: escape-dense messages must classify well under the
# 10s hook timeout (the scanner must stay linear, not quadratic).
out=$(python3 - "$H" "$D" <<'EOF'
import json, os, subprocess, sys
text = 'w"w ' * 20000 + chr(10) + "closing line of words " * 30
payload = json.dumps({"stop_hook_active": False, "last_assistant_message": text})
env = {**os.environ, "HOME": sys.argv[1], "CLAUDE_PLUGIN_DATA": sys.argv[2]}
try:
    r = subprocess.run(["bash", "../hooks/stop.sh"], input=payload,
                       capture_output=True, text=True, timeout=5, env=env)
    print(r.stdout.strip())
except subprocess.TimeoutExpired:
    print("TIMEOUT")
EOF
)
assert_block "20k escaped quotes classify in time" "$out"

# --- Stop hook: sentinel written only when a TL;DR is actually requested ---
rm -rf "$D"
printf '{"briefTranscript": true}' >"$H/.claude.json"
build_stop_input "$short_msg" | run_stop "$H" "$D" >/dev/null
[ -f "$D/seen-focus" ] && s=created || s=missing
check "focus on without a block: no sentinel" "$s" "missing"

build_stop_input "$long_multiline" | run_stop "$H" "$D" >/dev/null
[ -f "$D/seen-focus" ] && s=created || s=missing
check "focus on with a block: sentinel created" "$s" "created"

rm -rf "$D"
printf '{"other": 1}' >"$H/.claude.json"
build_stop_input "$long_multiline" | run_stop "$H" "$D" >/dev/null
[ -f "$D/seen-focus" ] && s=created || s=missing
check "focus off with a block: no sentinel" "$s" "missing"

rm -f "$H/.claude.json"
out=$(build_stop_input "$long_multiline" | run_stop "$H" "$D")
assert_block "missing claude.json still classifies" "$out"

out=$(build_stop_input "$long_multiline" | run_stop "$H" "UNSET")
assert_block "unset CLAUDE_PLUGIN_DATA still classifies" "$out"

# --- SessionStart hook ---
# run_start <home> <plugin_data or "UNSET">; prints "<decoded sysmsg>|<ctx-ok>"
run_start() {
  local out
  if [ "$2" = "UNSET" ]; then
    out=$(env -u CLAUDE_PLUGIN_DATA HOME="$1" bash "$START")
  else
    out=$(env CLAUDE_PLUGIN_DATA="$2" HOME="$1" bash "$START")
  fi
  printf '%s' "$out" | python3 -c '
import json, sys
o = json.load(sys.stdin)
h = o["hookSpecificOutput"]
assert h["hookEventName"] == "SessionStart", h
expected = "When the Stop hook asks you to TL;DR your last message, reply with one plain sentence and no \"TL;DR:\" prefix. Don'"'"'t shorten or pre-summarize messages to preempt the hook — the separate TL;DR message is what lets /focus toggle between the summary and the full message. If the user asks for more detail — especially detail you'"'"'ve already given — they may be seeing only the TL;DRs; tell them to turn /focus off."
ctx_ok = "ctx-ok" if h["additionalContext"] == expected else "ctx-BAD:" + repr(h["additionalContext"])
print(o.get("systemMessage", "NONE") + "|" + ctx_ok)
'
}

# Expected nudges, decoded: json.load turns the textual escapes into real ESC.
esc=$(printf '\033')
B="${esc}[1m"
M="${esc}[1;35m"
R="${esc}[0m"
NUDGE_FOCUS="${B}tldr:${R} run ${M}/focus${R} to collapse long replies to their TL;DRs|ctx-ok"
NUDGE_TUI="${B}tldr:${R} run ${M}/tui fullscreen${R}, then ${M}/focus${R} to collapse long replies to their TL;DRs|ctx-ok"

rm -rf "$D" "$H"
mkdir -p "$H"
out=$(run_start "$H" "$D")
check "no sentinel, no fullscreen: /tui nudge" "$out" "$NUDGE_TUI"

mkdir -p "$H/.claude"
printf '{"tui": "fullscreen"}' >"$H/.claude/settings.json"
out=$(run_start "$H" "$D")
check "no sentinel, fullscreen: /focus nudge" "$out" "$NUDGE_FOCUS"

printf '{"briefTranscript": true}' >"$H/.claude.json"
out=$(run_start "$H" "$D")
check "focus already on: no nudge" "$out" "NONE|ctx-ok"
[ -f "$D/seen-focus" ] && s=created || s=missing
check "focus already on: sentinel NOT written" "$s" "missing"

rm -f "$H/.claude.json"
mkdir -p "$D" && touch "$D/seen-focus"
out=$(run_start "$H" "$D")
check "sentinel present: no nudge" "$out" "NONE|ctx-ok"

out=$(run_start "$H" "UNSET")
check "unset CLAUDE_PLUGIN_DATA still nudges + injects" "$out" "$NUDGE_FOCUS"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
