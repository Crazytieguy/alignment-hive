#!/usr/bin/env bash
# Prints each plugin dir whose shipped files changed after the last commit that
# changed its `plugin.json` version, as of REV (default HEAD). Promote to stable
# holds `stable` while it prints anything, so a fresh install never gets content
# under a version that was released without it, and the release skill's
# unreleased.sh lists plugins by the same rule. Run from the repo root.
#
# Sourced, it only defines shipped_pathspec.

# Sets SHIPPED to the pathspecs of what a plugin ships: its dir, less the files
# that never change what a user's install does (tests and CLAUDE.md files).
shipped_pathspec() { # plugin dir, no trailing slash
  SHIPPED=("$1" ":(exclude)$1/tests" ":(exclude,glob)$1/**/*.test.ts" ":(exclude,glob)$1/**/*.test.tsx"
    ":(exclude,glob)$1/**/*.spec.ts" ":(exclude,glob)$1/**/CLAUDE.md")
}

main() {
  set -euo pipefail
  local rev=${1:-HEAD} manifest dir bump
  for manifest in plugins/*/.claude-plugin/plugin.json; do
    dir=${manifest%/.claude-plugin/plugin.json}
    bump=$(git log -1 --format=%H -G'"version"' "$rev" -- "$manifest")
    [ -n "$bump" ] || continue # not committed at REV
    shipped_pathspec "$dir"
    git diff --quiet "$bump" "$rev" -- "${SHIPPED[@]}" || echo "$dir"
  done
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi
