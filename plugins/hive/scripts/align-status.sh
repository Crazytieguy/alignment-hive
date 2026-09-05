#!/bin/bash
set -euo pipefail
# Status block for /hive:align.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
source "$SCRIPT_DIR/common.sh"

STATE_DIR="$(resolve_state_dir "$PWD")"
PLUGIN_VERSION="$(plugin_version "$PLUGIN_ROOT")"
LAST_VERSION=$(cat "$STATE_DIR/align-version" 2>/dev/null || echo "never run")

echo "**Plugin version**: ${PLUGIN_VERSION:-unknown}"
echo "**Last run version**: $LAST_VERSION"
echo "**State dir**: $STATE_DIR"

# Platform-specific marketplace entries for the plugins that ship a binary.
# Resolved here rather than in the command prose so the target triple has one
# definition and the catalog lookup is exact.
case "$(uname -s 2>/dev/null)" in
  Linux) OS_TRIPLE="unknown-linux-gnu" ;;
  Darwin) OS_TRIPLE="apple-darwin" ;;
  *) OS_TRIPLE="" ;;
esac
case "$(uname -m 2>/dev/null)" in
  x86_64) ARCH_TRIPLE="x86_64" ;;
  aarch64 | arm64) ARCH_TRIPLE="aarch64" ;;
  *) ARCH_TRIPLE="" ;;
esac

CATALOG="$HOME/.claude/plugins/marketplaces/alignment-hive/.claude-plugin/marketplace.json"
if [ -z "$OS_TRIPLE" ] || [ -z "$ARCH_TRIPLE" ]; then
  echo "**Platform entry suffix**: none (unsupported platform)"
  echo "**Platform entries available for**: none"
elif [ ! -f "$CATALOG" ]; then
  echo "**Platform entry suffix**: -${ARCH_TRIPLE}-${OS_TRIPLE}"
  echo "**Platform entries available for**: unknown (alignment-hive marketplace catalog not found; add the marketplace first)"
else
  SUFFIX="-${ARCH_TRIPLE}-${OS_TRIPLE}"
  AVAILABLE=""
  for plugin in model-router remote-kernels; do
    if grep -q "\"${plugin}${SUFFIX}\"" "$CATALOG"; then
      AVAILABLE="$AVAILABLE $plugin"
    fi
  done
  echo "**Platform entry suffix**: $SUFFIX"
  echo "**Platform entries available for**:${AVAILABLE:- none}"
fi

echo
echo "## Previously Rejected"
echo
cat "$STATE_DIR/align-rejected.md" 2>/dev/null || echo "(none recorded)"
