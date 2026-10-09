#!/usr/bin/env bash
# Lists every releasable component with commits since its version was last
# changed on origin/main. A version already bumped in an unpushed commit is
# reported as such, so it ships as-is rather than being bumped again. Run from
# the repo root after pulling.

version_of() { # rev, file, pattern
  git show "$1:$2" 2>/dev/null | grep -m1 -E "$3" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+'
}

report() { # name, version file, version pattern, paths...
  local name=$1 file=$2 pattern=$3; shift 3
  local base released current log
  base=$(git log -1 --format=%h -G"$pattern" origin/main -- "$file")
  released=$(version_of origin/main "$file" "$pattern")
  current=$(version_of HEAD "$file" "$pattern")
  if [ -z "$base" ]; then
    echo "## $name $current (never released)"
    echo
    return
  fi
  log=$(git log --format='  %h %s' "$base"..HEAD -- "$@")
  [ -z "$log" ] && return
  if [ "$current" = "$released" ]; then
    echo "## $name $released (bumped in $base)"
  else
    echo "## $name $released (bumped in $base); already $current in an unpushed commit, ship as-is"
  fi
  echo "$log"
  git diff --stat=100 "$base"..HEAD -- "$@" | sed 's/^/  /'
  echo
}

for dir in plugins/*/; do
  name=$(basename "$dir")
  [ -f "$dir.claude-plugin/plugin.json" ] || continue
  report "plugin $name" "$dir.claude-plugin/plugin.json" '"version"' "$dir"
done
for dir in crates/*/; do
  [ -f "${dir}Cargo.toml" ] || continue
  report "crate $(basename "$dir")" "${dir}Cargo.toml" '^version' "$dir"
done
# The paths that trigger .github/workflows/release-hive-cli.yml.
report "hive-cli" packages/hive-cli/package.json '"version"' \
  packages/hive-cli/src packages/session-data/src packages/ui/src \
  packages/review-app/src packages/review-app/index.html
