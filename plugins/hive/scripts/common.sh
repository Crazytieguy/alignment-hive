# Sourced by session-start.sh and align-status.sh.

# The CLI's state dir: <main worktree>/.claude/hive (resolved from the cwd), or $1/.claude/hive outside a repo.
resolve_state_dir() {
  local main_worktree
  main_worktree=$(git worktree list --porcelain 2>/dev/null | head -1 | sed 's/^worktree //' || echo "")
  echo "${main_worktree:-$1}/.claude/hive"
}

# The plugin's version from its manifest; empty when unreadable.
plugin_version() {
  jq -r '.version // ""' "$1/.claude-plugin/plugin.json" 2>/dev/null || echo ""
}
