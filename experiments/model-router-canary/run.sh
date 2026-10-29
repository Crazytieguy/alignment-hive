#!/bin/sh
# Launched hourly by launchd (install.sh). `canary.ts tick` returns at once unless something is due: the live checks
# once a day or when Claude Code or the router changed, a new CLIProxyAPI release once it is old enough. When a run
# needs attention (exit 2) or the canary itself failed (exit 1), starts a background Claude Code session on the
# report, in this repository, in auto mode. A quiet run starts no session.
# install.sh passes the installing shell's PATH; the usual install locations back it up.
export PATH="${PATH:+$PATH:}$HOME/.local/bin:$HOME/.bun/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/../.." || exit 1
data="${CANARY_DATA:-$HOME/.local/share/alignment-hive-model-router-canary}"
mkdir -p "$data"

bun "$here/canary.ts" tick > "$data/last-stdout.txt"
code=$?
[ "$code" = 0 ] && exit 0

report="$(tail -n 1 "$data/last-stdout.txt")"
if [ ! -f "$report" ]; then
  # A crash before the report was written leaves no path; the launchd log then holds the error. It would recur every
  # hour, so it starts one session a day.
  [ -n "$(find "$data/crash-session" -mtime -1 2>/dev/null)" ] && exit 1
  touch "$data/crash-session"
  report="$data/launchd.log (the canary exited $code before writing a report)"
fi
prompt="$(sed "s|{{REPORT}}|$report|g" "$here/session-prompt.md")"
exec claude --bg --name model-router-canary --permission-mode auto "$prompt"
