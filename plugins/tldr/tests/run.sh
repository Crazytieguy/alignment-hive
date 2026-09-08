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

# hook_env <home> <plugin_data or "UNSET"> <cmd...> — run with only the test's
# HOME/data dir; the real CLAUDE_CONFIG_DIR and project dir must not leak in.
hook_env() {
  local home=$1 data=$2
  shift 2
  if [ "$data" = "UNSET" ]; then
    env -u CLAUDE_PLUGIN_DATA -u CLAUDE_CONFIG_DIR CLAUDE_PROJECT_DIR="$TMP/proj" HOME="$home" "$@"
  else
    env -u CLAUDE_CONFIG_DIR CLAUDE_PLUGIN_DATA="$data" CLAUDE_PROJECT_DIR="$TMP/proj" HOME="$home" "$@"
  fi
}

# run_stop <home> <plugin_data or "UNSET"> — stdin passes through to the hook
run_stop() { hook_env "$1" "$2" bash "$STOP"; }

check() { # <name> <actual> <expected>
  if [ "$2" = "$3" ]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    printf 'FAIL %s\n  expected: %q\n  actual:   %q\n' "$1" "$3" "$2"
  fi
}

# The block reason's wording is not pinned; what matters is valid JSON with a
# block decision, and for markers, a reason that starts the TL;DR with the marker.
# decode_block <output> [marker] prints "block" / "block:<marker>" / "BAD:<...>".
decode_block() {
  printf '%s' "$1" | python3 -c '
import json, sys
try:
    o = json.load(sys.stdin)
except Exception as e:
    print("BAD:not json: " + repr(sys.argv[1][:80])); sys.exit()
if o.get("decision") != "block":
    print("BAD:" + repr(o)); sys.exit()
marker = sys.argv[2]
if marker and chr(34) + marker + ":" + chr(34) not in o.get("reason", ""):
    print("BAD:reason lacks marker: " + repr(o.get("reason"))); sys.exit()
print("block" + (":" + marker if marker else ""))
' "$1" "${2:-}"
}
assert_block() { check "$1" "$(decode_block "$2")" "block"; }
assert_silent() { check "$1" "$2" ""; }
assert_sentinel() { # <name> <created|missing>
  local s
  [ -f "$D/seen-focus" ] && s=created || s=missing
  check "$1" "$s" "$2"
}

H="$TMP/home"
D="$TMP/data"
mkdir -p "$H" "$TMP/proj"

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

# The key is located with a 4096-char sliding window; a key that straddles a
# window boundary must still be found (removing the overlap fails this open).
# The padding is sized so the key starts 6 chars before the boundary.
out=$(build_stop_input "$long_multiline" | python3 -c '
import json, sys
o = json.load(sys.stdin)
rest = json.dumps(o)[1:]  # drop the opening brace
head = "{" + json.dumps("a_pad") + ": "
key = json.dumps("last_assistant_message")
i0 = (head + json.dumps("") + ", " + rest).index(key)
s = head + json.dumps("p" * (4090 - i0)) + ", " + rest
i = s.index(key)
assert i < 4096 < i + 24, (i, "key does not straddle the boundary")
print(s)' | run_stop "$H" "$D")
assert_block "key straddling a 4096-char window boundary is found" "$out"

# --- Stop hook: background-session markers carried into the TL;DR request ---
# A line starting with "needs input:", "result:" or "failed:" (the job
# classifier's markers) makes the request ask for a TL;DR starting with it.
assert_marked() { check "$1" "$(decode_block "$2" "$3")" "block:$3"; }

out=$(build_stop_input "'${W101}'+chr(10)+'needs input: which office?'" | run_stop "$H" "$D")
assert_marked "needs input on the last line" "$out" "needs input"

out=$(build_stop_input "'${W101}'+chr(10)+'result: the fix is landed.'+chr(10)" | run_stop "$H" "$D")
assert_marked "result line followed by a newline" "$out" "result"

out=$(build_stop_input "'${W101}'+chr(10)+'failed: wrong repo.'" | run_stop "$H" "$D")
assert_marked "failed line" "$out" "failed"

out=$(build_stop_input "'result: first line headline'+chr(10)+'${W101}'" | run_stop "$H" "$D")
assert_marked "marker on the first line" "$out" "result"

out=$(build_stop_input "'${W101}'+chr(10)+'  Needs Input :  which office?'" | run_stop "$H" "$D")
assert_marked "marker is case-insensitive with blanks around it" "$out" "needs input"

out=$(build_stop_input "'${W101}'+chr(10)+chr(9)+'result: tabbed'" | run_stop "$H" "$D")
assert_marked "encoded tab before the marker" "$out" "result"

out=$(build_stop_input "'${W101}'+chr(10)+'result: one'+chr(10)+'needs input: two'" | run_stop "$H" "$D")
assert_marked "last marker wins (needs input after result)" "$out" "needs input"

out=$(build_stop_input "'${W101}'+chr(10)+'needs input: one'+chr(10)+'result: two'" | run_stop "$H" "$D")
assert_marked "last marker wins (result after needs input)" "$out" "result"

out=$(build_stop_input "'${W101}'+chr(10)+'The result: was fine.'" | run_stop "$H" "$D")
assert_block "marker mid-line is not a marker" "$out"

out=$(build_stop_input "'${W101}'+chr(10)+'results: all green'" | run_stop "$H" "$D")
assert_block "a longer word is not a marker" "$out"

out=$(build_stop_input "'${W101}'+chr(10)+'needs input from you on this'" | run_stop "$H" "$D")
assert_block "marker word without a colon is not a marker" "$out"

out=$(build_stop_input "'${W101}'+chr(10)+'blocked: not a documented marker'" | run_stop "$H" "$D")
assert_block "blocked: is not carried" "$out"

out=$(build_stop_input "'${W101}'+chr(10)+'\"result:\" is escaped'" | run_stop "$H" "$D")
assert_block "escaped quote before the word is not a marker" "$out"

out=$(build_stop_input "'short reply.'+chr(10)+'needs input: still short'" | run_stop "$H" "$D")
assert_silent "marker in a short message: no TL;DR at all" "$out"

# Performance regression: escape-dense messages must classify well under the
# 10s hook timeout (the scanner must stay linear, not quadratic).
out=$(python3 - "$H" "$D" "$STOP" <<'EOF'
import json, os, subprocess, sys
text = 'w"w ' * 20000 + chr(10) + "closing line of words " * 30
payload = json.dumps({"stop_hook_active": False, "last_assistant_message": text})
env = {**os.environ, "HOME": sys.argv[1], "CLAUDE_PLUGIN_DATA": sys.argv[2]}
try:
    r = subprocess.run(["bash", sys.argv[3]], input=payload,
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
assert_sentinel "focus on without a block: no sentinel" "missing"

build_stop_input "$long_multiline" | run_stop "$H" "$D" >/dev/null
assert_sentinel "focus on with a block: sentinel created" "created"

rm -rf "$D"
printf '{"other": 1}' >"$H/.claude.json"
build_stop_input "$long_multiline" | run_stop "$H" "$D" >/dev/null
assert_sentinel "focus off with a block: no sentinel" "missing"

# CLAUDE_CONFIG_DIR relocates .claude.json; focus state must be read from there.
rm -rf "$D"
mkdir -p "$TMP/cfg"
printf '{"briefTranscript": true}' >"$TMP/cfg/.claude.json"
build_stop_input "$long_multiline" | CLAUDE_CONFIG_DIR="$TMP/cfg" env CLAUDE_PLUGIN_DATA="$D" HOME="$H" bash "$STOP" >/dev/null
assert_sentinel "focus on under CLAUDE_CONFIG_DIR: sentinel created" "created"
rm -rf "$D" "$TMP/cfg"

rm -f "$H/.claude.json"
out=$(build_stop_input "$long_multiline" | run_stop "$H" "$D")
assert_block "missing claude.json still classifies" "$out"

out=$(build_stop_input "$long_multiline" | run_stop "$H" "UNSET")
assert_block "unset CLAUDE_PLUGIN_DATA still classifies" "$out"

# --- SessionStart hook ---
# run_start <home> <plugin_data or "UNSET">; prints "<nudge kind>|<ctx-ok>".
# Asserts structure, not wording: valid JSON, the hand-built escapes decoded
# (the quoted "TL;DR:" token survived, the nudge starts with a real ESC byte),
# and which nudge branch fired.
run_start() {
  hook_env "$1" "$2" bash "$START" | python3 -c '
import json, sys
o = json.load(sys.stdin)
h = o["hookSpecificOutput"]
assert h["hookEventName"] == "SessionStart", h
ctx = h["additionalContext"]
ctx_ok = "ctx-ok" if chr(34) + "TL;DR:" + chr(34) in ctx and "/focus" in ctx else "ctx-BAD:" + repr(ctx)
msg = o.get("systemMessage", "NONE")
if msg != "NONE":
    kind = "tui" if "/tui fullscreen" in msg else ("focus" if "/focus" in msg else "unknown")
    msg = ("esc-ok:" if msg.startswith(chr(27) + "[") else "esc-BAD:") + kind
print(msg + "|" + ctx_ok)
'
}
NUDGE_FOCUS="esc-ok:focus|ctx-ok"
NUDGE_TUI="esc-ok:tui|ctx-ok"

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
assert_sentinel "focus already on: sentinel NOT written" "missing"

rm -f "$H/.claude/settings.json"
mkdir -p "$TMP/proj/.claude"
printf '{"tui": "fullscreen"}' >"$TMP/proj/.claude/settings.local.json"
rm -f "$H/.claude.json"
out=$(run_start "$H" "$D")
check "fullscreen set in project settings: /focus nudge" "$out" "$NUDGE_FOCUS"
rm -f "$TMP/proj/.claude/settings.local.json"
printf '{"tui": "fullscreen"}' >"$H/.claude/settings.json"

rm -f "$H/.claude.json"
mkdir -p "$D" && touch "$D/seen-focus"
out=$(run_start "$H" "$D")
check "sentinel present: no nudge" "$out" "NONE|ctx-ok"

out=$(run_start "$H" "UNSET")
check "unset CLAUDE_PLUGIN_DATA still nudges + injects" "$out" "$NUDGE_FOCUS"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
