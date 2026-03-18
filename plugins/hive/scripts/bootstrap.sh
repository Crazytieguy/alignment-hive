#!/bin/bash
set -euo pipefail

# Bootstrap script for hive CLI binary.
# Downloads the prebuilt binary from GitHub releases (cached locally),
# then exec's it so signals propagate correctly.
# Falls back to last successfully used binary if download fails.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
CACHE_BASE="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/hive}"
LAST_USED_FILE="$CACHE_BASE/last-used"

# Read version from cli-version file
CLI_VERSION_FILE="$PLUGIN_ROOT/cli-version"
if [ ! -f "$CLI_VERSION_FILE" ]; then
  echo "cli-version file not found" >&2
  exit 1
fi
VERSION=$(cat "$CLI_VERSION_FILE" | tr -d '[:space:]')
if [ -z "$VERSION" ]; then
  echo "cli-version file is empty" >&2
  exit 1
fi

# Dev mode: use locally-built binary if ALIGNMENT_HIVE_DEV is set
if [ "${ALIGNMENT_HIVE_DEV:-}" = "1" ] && [ -n "${CLAUDE_PROJECT_DIR:-}" ] && [ -x "$CLAUDE_PROJECT_DIR/.dev/hive" ]; then
  exec "$CLAUDE_PROJECT_DIR/.dev/hive" "$@"
fi

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  ;;
  darwin) ;;
  *)      echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64)        ARCH_NAME="x64" ;;
  aarch64|arm64) ARCH_NAME="arm64" ;;
  *)             echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${OS}-${ARCH_NAME}"

# Cache directory for this version
CACHE_DIR="$CACHE_BASE/v${VERSION}"
BINARY="$CACHE_DIR/hive"

# Try to fall back to last successfully used binary
fallback_or_exit() {
  if [ -f "$LAST_USED_FILE" ]; then
    FALLBACK=$(cat "$LAST_USED_FILE")
    if [ -n "$FALLBACK" ] && [ -x "$FALLBACK" ] && [ "$FALLBACK" != "$BINARY" ]; then
      echo "Falling back to $(basename "$(dirname "$FALLBACK")")" >&2
      exec "$FALLBACK" "$@"
    fi
  fi
  exit 1
}

# Download if not cached
if [ ! -x "$BINARY" ]; then
  BINARY_NAME="hive-cli-${TARGET}"
  DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/hive-cli-v${VERSION}/${BINARY_NAME}"

  echo "Downloading hive-cli v${VERSION} for ${TARGET}..." >&2
  mkdir -p "$CACHE_DIR"

  if ! curl -fSL "$DOWNLOAD_URL" -o "$BINARY" 2>/dev/null; then
    echo "Failed to download hive-cli v${VERSION}" >&2
    rm -f "$BINARY"
    fallback_or_exit "$@"
  fi

  chmod +x "$BINARY"
  echo "Installed hive-cli v${VERSION}" >&2

  # Also install globally
  mkdir -p "$HOME/.local/bin"
  cp "$BINARY" "$HOME/.local/bin/hive"
fi

# Record this as the last known good binary
mkdir -p "$CACHE_BASE"
echo "$BINARY" > "$LAST_USED_FILE"

exec "$BINARY" "$@"
