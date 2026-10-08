# Plugins

**Auto-expanding bash commands fail hard.** If `` !`command` `` in a skill/agent/command returns non-zero, the entire file fails to load. Use fallbacks like `command 2>/dev/null || echo "fallback"`.

**Registering a plugin.** Add it to `.claude-plugin/marketplace.json` with a `name` matching its folder. The platform-specific archive entries for model-router and remote-kernels are the exception: their names carry a target triple and their urls are fixed, so they are written once.
