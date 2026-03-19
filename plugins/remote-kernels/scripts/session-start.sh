#!/bin/bash
# SessionStart hook: nudge users to set up remote-kernels if not configured.

# If remote-kernels.toml exists, setup is done — exit silently.
if [ -f "$CLAUDE_PROJECT_DIR/remote-kernels.toml" ]; then
  exit 0
fi

echo '{"systemMessage": "\u001b[1mremote-kernels:\u001b[0m not configured, run \u001b[1;35m/remote-kernels:setup\u001b[0m"}'
