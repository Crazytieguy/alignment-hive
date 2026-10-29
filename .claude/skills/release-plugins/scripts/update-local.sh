#!/usr/bin/env bash
# Pulls a release onto this machine right away instead of waiting for the
# background auto-update: refreshes the alignment-hive marketplace, then
# updates every installed alignment-hive plugin. Project-scoped installs are
# updated from their project directory; ones whose directory is gone are
# skipped. Sessions pick the new versions up on restart.

set -u
marketplace=alignment-hive
installed="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/plugins/installed_plugins.json"

claude plugin marketplace update "$marketplace" || exit 1

status=0
while IFS=$'\t' read -r plugin scope dir; do
  if [ -n "$dir" ] && [ ! -d "$dir" ]; then
    echo "Skipping $plugin ($scope scope): $dir no longer exists"
    continue
  fi
  (cd "${dir:-$HOME}" && claude plugin update "$plugin" --scope "$scope") || status=1
done < <(jq -r --arg m "@$marketplace" '
  .plugins | to_entries[] | select(.key | endswith($m))
  | .key as $plugin | .value[] | [$plugin, .scope, .projectPath // ""] | @tsv
' "$installed")
exit $status
