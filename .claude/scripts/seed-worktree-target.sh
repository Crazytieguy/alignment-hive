#!/usr/bin/env bash
# PreToolUse(Bash) hook: before the first cargo command in a worktree, seed its
# target/ with a copy-on-write clone of the main checkout's target/. A cold
# worktree build otherwise compiles every dependency into its own ~25 GB
# target/; the clone is near-instant, shares blocks until written, and leaves
# only the workspace crates to recompile. Only clones when the filesystem
# supports it (APFS clonefile, or reflinks on Linux); never falls back to a
# full copy. Idempotent, silent on success, never blocks.
set -uo pipefail

input=$(cat)
command -v jq >/dev/null || exit 0
cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
# cargo as a command word, so paths like ~/.cargo don't trigger a clone.
[[ "$cmd" =~ (^|[[:space:]\;\&\|\(])cargo([[:space:]]|$) ]] || exit 0
cwd=$(jq -r '.cwd // empty' <<<"$input")
[[ -n "$cwd" ]] || exit 0

here=$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null) || exit 0
common=$(git -C "$cwd" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
main=$(dirname "$common")
[[ "$here" != "$main" ]] || exit 0
[[ ! -e "$here/target" && -d "$main/target" ]] || exit 0

tmp="$here/target.seeding.$$"
case "$(uname -s)" in
  Darwin) cp -Rc "$main/target" "$tmp" 2>/dev/null ;;
  Linux) cp -Rp --reflink=always "$main/target" "$tmp" 2>/dev/null ;;
  *) false ;;
esac || { rm -rf "$tmp"; exit 0; }

# A concurrent cargo command may have seeded or built target/ meanwhile, and
# mv onto an existing directory would nest the clone inside it.
if [[ -e "$here/target" ]]; then rm -rf "$tmp"; else mv "$tmp" "$here/target"; fi
exit 0
