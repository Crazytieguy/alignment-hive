#!/bin/bash
set -euo pipefail

# Runs the hive CLI this plugin pins, with all arguments and stdin passed through: the session
# start hook and the plugin's band (hooks/register.tsx) both go through here. Needs
# CLAUDE_PLUGIN_ROOT, and CLAUDE_PROJECT_DIR for the dev binary.

source "${CLAUDE_PLUGIN_ROOT}/scripts/common.sh"

# `hive notices` reads it for the /hive:align nudge.
export HIVE_PLUGIN_VERSION="$(plugin_version "$CLAUDE_PLUGIN_ROOT")"

# Dev binary shortcircuit: use the locally-built binary when running from the repo
if [[ "$CLAUDE_PLUGIN_ROOT" == "${CLAUDE_PROJECT_DIR:-}"/* ]] && [ -x "$CLAUDE_PROJECT_DIR/.dev/hive" ]; then
  exec "$CLAUDE_PROJECT_DIR/.dev/hive" "$@"
fi
# Bootstrap: ensure the correct CLI version is cached, updated, and exec'd
exec "${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh" "$@"
