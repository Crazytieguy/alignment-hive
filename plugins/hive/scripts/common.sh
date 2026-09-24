# Sourced by session-start.sh, register-transcript-dir.sh and align-status.sh.

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

# Register the directory holding a transcript (hook input's transcript_path) in the state dir's
# transcripts-dirs registry, which `hive local` and uploads read. Derived from transcript_path
# rather than recomputing Claude Code's project-dir naming (which sanitizes and truncates; see
# toClaudeProjectDirName). Append-only and deduped on read, so concurrent hooks cannot clobber
# each other. No-op for an empty path or a dir that does not exist yet. The CLI's writer of the same
# file is addTranscriptsDirs in packages/hive-cli/src/lib/config.ts; keep the two formats in step.
register_transcript_dir() {
  local state_dir="$1" transcript_path="$2" transcript_dir transcripts_file
  [ -n "$transcript_path" ] || return 0
  transcript_dir=$(dirname "$transcript_path")
  [ -d "$transcript_dir" ] || return 0
  transcripts_file="$state_dir/transcripts-dirs"
  if ! grep -qxF "$transcript_dir" "$transcripts_file" 2>/dev/null; then
    echo "$transcript_dir" >> "$transcripts_file"
  fi
}
