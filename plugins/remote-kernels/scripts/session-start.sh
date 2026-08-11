#!/bin/bash
# SessionStart hook: nudge users to set up remote-kernels if not configured,
# and (once per binary-version change) to re-run setup after an update so
# they can pick their data-preservation strategy.

STATE_DIR="$CLAUDE_PROJECT_DIR/.claude/remote-kernels"
BINARY_VERSION=$(cat "${CLAUDE_PLUGIN_ROOT}/binary-version" 2>/dev/null || echo "")

# Wrong platform-specific variant installed: the MCP server cannot start, so
# say that instead of a setup nudge. bootstrap.sh owns the test.
if ! bash "${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh" platform-check >/dev/null 2>&1; then
  echo '{"systemMessage": "\u001b[1mremote-kernels:\u001b[0m wrong platform, run \u001b[1;35m/remote-kernels:setup\u001b[0m to fix"}'
  exit 0
fi

# If remote-kernels.toml exists, setup is done — but nudge once after updates.
if [ -f "$CLAUDE_PROJECT_DIR/remote-kernels.toml" ]; then
  if [ -n "$BINARY_VERSION" ]; then
    MARKER="$STATE_DIR/nudged-version"
    LAST=$(cat "$MARKER" 2>/dev/null || echo "")
    if [ "$LAST" != "$BINARY_VERSION" ]; then
      mkdir -p "$STATE_DIR" 2>/dev/null && echo "$BINARY_VERSION" > "$MARKER"
      # No prior marker means the plugin predates the nudge mechanism; for an
      # already-configured project that IS the update case, so nudge unless
      # the project shows no prior machine state at all.
      if [ -n "$LAST" ] || [ -d "$STATE_DIR/instances" ] || [ -d "$STATE_DIR/ledger" ]; then
        echo '{"systemMessage": "\u001b[1mremote-kernels:\u001b[0m updated, re-run \u001b[1;35m/remote-kernels:setup\u001b[0m"}'
      fi
    fi
  fi
  exit 0
fi

echo '{"systemMessage": "\u001b[1mremote-kernels:\u001b[0m not configured, run \u001b[1;35m/remote-kernels:setup\u001b[0m"}'
