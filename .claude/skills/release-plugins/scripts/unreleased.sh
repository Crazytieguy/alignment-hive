#!/usr/bin/env bash
# Lists every releasable component with commits since its version was last
# changed. Run from the repo root.

report() { # name, version file, version pattern, paths...
  local name=$1 file=$2 pattern=$3; shift 3
  local base version log
  base=$(git log -1 --format=%h -G"$pattern" -- "$file")
  version=$(grep -m1 -E "$pattern" "$file" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
  log=$(git log --format='  %h %s' "$base"..HEAD -- "$@")
  [ -z "$log" ] && return
  echo "## $name $version (bumped in $base)"
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
