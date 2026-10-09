#!/bin/bash
# Run once per session by hooks/register.tsx: prints the band's rows as JSON
# ({"notices": [...]}, the Notice shape in hooks/notice-band.tsx), or nothing.
# Nudges users to set up remote-kernels if not configured. An update asks
# nothing of a configured project, so it gets no row.

# One row whose button runs /remote-kernels:setup: id, severity, text, button label.
setup_notice() {
  printf '{"notices": [{"id": "%s", "severity": "%s", "text": "%s", "actions": [{"id": "setup", "label": "%s", "kind": "command", "command": "/remote-kernels:setup"}]}]}\n' "$1" "$2" "$3" "$4"
}

# Wrong platform-specific variant installed: the MCP server cannot start, so
# say that instead of a setup nudge. bootstrap.sh owns the test.
if ! bash "${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh" platform-check >/dev/null 2>&1; then
  setup_notice platform problem "wrong platform variant installed" "Fix"
  exit 0
fi

if [ ! -f "$CLAUDE_PROJECT_DIR/remote-kernels.toml" ]; then
  setup_notice setup action "not configured" "Run setup"
fi
