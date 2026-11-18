#!/bin/sh
# Launched nightly by launchd (install.sh). Lists issues and PRs other people opened since the last triage; when there
# are any, starts a background Claude Code session in this repository to triage them. A quiet night starts no session.
# install.sh passes the installing shell's PATH; the usual install locations back it up.
export PATH="${PATH:+$PATH:}$HOME/.local/bin:$HOME/.bun/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/../.." || exit 1

items="$(bun "$here/new-items.ts")" || exit 1
[ -n "$items" ] || exit 0

prompt="$(cat "$here/prompt.md")

$items"
exec claude --bg --name triage --permission-mode auto "$prompt"
