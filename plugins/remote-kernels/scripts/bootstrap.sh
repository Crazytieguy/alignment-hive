#!/bin/bash
set -euo pipefail

# Bootstrap script for remote-kernels MCP server.
# Downloads the prebuilt binary from GitHub releases (cached locally),
# then exec's it so signals propagate correctly for graceful shutdown.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"

# Read version from plugin.json (no jq dependency)
VERSION=$(grep '"version"' "$PLUGIN_ROOT/.claude-plugin/plugin.json" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
if [ -z "$VERSION" ]; then
  echo "Failed to read version from plugin.json" >&2
  exit 1
fi

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OS_TRIPLE="unknown-linux-gnu" ;;
  darwin) OS_TRIPLE="apple-darwin" ;;
  *)      echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64)        ARCH_TRIPLE="x86_64" ;;
  aarch64|arm64) ARCH_TRIPLE="aarch64" ;;
  *)             echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${ARCH_TRIPLE}-${OS_TRIPLE}"

# Dev mode: use locally-built binary if REMOTE_KERNELS_DEV is set
if [ -n "${REMOTE_KERNELS_DEV:-}" ] && [ -x "$REMOTE_KERNELS_DEV" ]; then
  exec "$REMOTE_KERNELS_DEV" "$@"
fi

# Cache directory
CACHE_DIR="$HOME/.cache/remote-kernels/v${VERSION}"
BINARY="$CACHE_DIR/remote-kernels"

# Download if not cached
if [ ! -x "$BINARY" ]; then
  ARCHIVE_NAME="remote-kernels-${TARGET}.tar.xz"
  DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/remote-kernels-v${VERSION}/${ARCHIVE_NAME}"

  echo "Downloading remote-kernels v${VERSION} for ${TARGET}..." >&2
  mkdir -p "$CACHE_DIR"

  if ! curl -fSL "$DOWNLOAD_URL" -o "$CACHE_DIR/$ARCHIVE_NAME" 2>/dev/null; then
    echo "Failed to download from: $DOWNLOAD_URL" >&2
    echo "Check that v${VERSION} has been released with binaries for ${TARGET}" >&2
    exit 1
  fi

  tar -xf "$CACHE_DIR/$ARCHIVE_NAME" -C "$CACHE_DIR"
  rm -f "$CACHE_DIR/$ARCHIVE_NAME"

  # cargo-dist archives nest the binary in a subdirectory
  if [ ! -f "$BINARY" ]; then
    FOUND=$(find "$CACHE_DIR" -name "remote-kernels" -type f 2>/dev/null | head -1)
    if [ -n "$FOUND" ]; then
      mv "$FOUND" "$BINARY"
      # Clean up extracted subdirectories
      find "$CACHE_DIR" -mindepth 1 -type d -exec rm -rf {} + 2>/dev/null || true
    else
      echo "Binary not found in archive" >&2
      exit 1
    fi
  fi

  chmod +x "$BINARY"
  echo "Installed remote-kernels v${VERSION}" >&2
fi

exec "$BINARY" "$@"
