#!/bin/bash
# Sourceable snippet that sets DENO to a deno binary path.
# Returns 1 if deno is not available (caller decides how to handle).
# Usage: source "${CLAUDE_PLUGIN_ROOT}/scripts/find-deno.sh" || exit 0

if command -v deno >/dev/null 2>&1; then
  DENO="deno"
elif [ -x "$HOME/.deno/bin/deno" ]; then
  DENO="$HOME/.deno/bin/deno"
else
  return 1
fi
