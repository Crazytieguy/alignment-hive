#!/bin/bash
set -euo pipefail

# Bootstrap script for remote-kernels MCP server.
# Resolves the prebuilt binary (cached locally), then exec's it so signals
# propagate correctly for graceful shutdown.
#
# The platform-specific marketplace entries ship the binary inside the plugin
# zip, at bin/remote-kernels-<target>.tar.xz. When it is there the binary comes
# from the plugin itself, so plugin and binary are never out of step. The plain
# (path-source) plugin has no bin/ and downloads from GitHub releases as before.
# A bin/ that holds some other platform's binary is an error, not a fallback.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"

# Read the binary release version from binary-version. This is deliberately
# decoupled from plugin.json's version: the plugin version bumps on any content
# change (skills, scripts), while binary-version only changes when a new crate
# binary is released. Same pattern as plugins/hive/cli-version.
VERSION=$(tr -d '[:space:]' < "$PLUGIN_ROOT/binary-version")
if [ -z "$VERSION" ]; then
  echo "Failed to read version from binary-version" >&2
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

# A platform-specific plugin zip carries exactly one target's binary. A bin/
# without ours means the wrong variant is installed: stop, rather than quietly
# downloading, which would hand back the plugin/binary skew the bundle exists
# to remove. `platform-check` runs just this test — no download, no exec — and
# prints the entry that should have been installed, for the setup skill.
if [ -d "$PLUGIN_ROOT/bin" ] && [ ! -f "$PLUGIN_ROOT/bin/remote-kernels-${TARGET}.tar.xz" ]; then
  if [ "${1:-}" = "platform-check" ]; then
    echo "remote-kernels-${TARGET}"
  fi
  echo "remote-kernels: wrong platform, run /remote-kernels:setup to fix" >&2
  exit 1
fi
if [ "${1:-}" = "platform-check" ]; then
  exit 0
fi

# Dev mode: use locally-built binary if REMOTE_KERNELS_DEV is set
if [ -n "${REMOTE_KERNELS_DEV:-}" ] && [ -x "$REMOTE_KERNELS_DEV" ]; then
  exec "$REMOTE_KERNELS_DEV" "$@"
fi

# Cache directory
CACHE_DIR="$HOME/.cache/remote-kernels/v${VERSION}"
BINARY="$CACHE_DIR/remote-kernels"

# Populate the cache if it is empty: from the bundled archive when the plugin
# ships one, otherwise from GitHub releases.
if [ ! -x "$BINARY" ]; then
  ARCHIVE_NAME="remote-kernels-${TARGET}.tar.xz"
  # Present only in the platform-specific plugin zips; a mismatched one already
  # exited above, so reaching here with no bundle means the plain plugin.
  ARCHIVE="$PLUGIN_ROOT/bin/$ARCHIVE_NAME"

  mkdir -p "$CACHE_DIR"

  if [ ! -f "$ARCHIVE" ]; then
    ARCHIVE="$CACHE_DIR/$ARCHIVE_NAME"
    DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/remote-kernels-v${VERSION}/${ARCHIVE_NAME}"

    echo "Downloading remote-kernels v${VERSION} for ${TARGET}..." >&2

    if ! curl -fSL "$DOWNLOAD_URL" -o "$ARCHIVE" 2>/dev/null; then
      echo "Failed to download from: $DOWNLOAD_URL" >&2
      echo "Check that v${VERSION} has been released with binaries for ${TARGET}" >&2
      exit 1
    fi
  fi

  tar -xf "$ARCHIVE" -C "$CACHE_DIR"
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
