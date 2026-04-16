# Plugins

**Auto-expanding bash commands fail hard.** If `` !`command` `` in a skill/agent/command returns non-zero, the entire file fails to load. Use fallbacks like `command 2>/dev/null || echo "fallback"`.
