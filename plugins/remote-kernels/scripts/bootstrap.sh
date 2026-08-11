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
# Written only after the binary is fully in place. An executable at $BINARY is
# not by itself proof of a complete install: bootstrap before plugin 0.2.6
# extracted into the live cache dir, so a lost race could leave a truncated
# file that `-x` accepts forever, with no way back short of deleting the cache
# by hand. Caches from those versions have no stamp and are reinstalled once.
STAMP="$CACHE_DIR/.installed"

# Populate the cache if it is empty: from the bundled archive when the plugin
# ships one, otherwise from GitHub releases.
if [ ! -x "$BINARY" ] || [ ! -f "$STAMP" ]; then
  # Download and extract into a private staging dir, then atomically rename
  # the binary into place: Claude Code runs this once per session and several
  # sessions can start at once, so extracting into the live cache dir would
  # let one run publish another's half-written file. $BINARY is only ever a
  # complete file or absent, and a mid-extract kill leaves nothing behind.
  ARCHIVE_NAME="remote-kernels-${TARGET}.tar.xz"
  # Present only in the platform-specific plugin zips; a mismatched one already
  # exited above, so reaching here with no bundle means the plain plugin.
  ARCHIVE="$PLUGIN_ROOT/bin/$ARCHIVE_NAME"

  mkdir -p "$CACHE_DIR"
  # Reap staging dirs orphaned by killed runs; only clearly-stale ones so a
  # concurrent extraction is never disturbed.
  find "$CACHE_DIR" -maxdepth 1 -name 'staging.*' -mmin +60 -exec rm -rf {} + 2>/dev/null || true
  STAGING=$(mktemp -d "${CACHE_DIR}/staging.XXXXXX")
  trap 'rm -rf "$STAGING"' EXIT

  if [ ! -f "$ARCHIVE" ]; then
    ARCHIVE="$STAGING/$ARCHIVE_NAME"
    DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/remote-kernels-v${VERSION}/${ARCHIVE_NAME}"

    echo "Downloading remote-kernels v${VERSION} for ${TARGET}..." >&2

    if ! curl -fSL "$DOWNLOAD_URL" -o "$ARCHIVE" 2>/dev/null; then
      echo "Failed to download from: $DOWNLOAD_URL" >&2
      echo "Check that v${VERSION} has been released with binaries for ${TARGET}" >&2
      exit 1
    fi
  fi

  tar -xf "$ARCHIVE" -C "$STAGING"

  # cargo-dist archives nest the binary in a subdirectory
  FOUND=$(find "$STAGING" -name "remote-kernels" -type f 2>/dev/null | head -1)
  if [ -z "$FOUND" ]; then
    echo "Binary not found in archive" >&2
    exit 1
  fi

  chmod +x "$FOUND"
  mv -f "$FOUND" "$BINARY"
  : > "$STAMP"
  rm -rf "$STAGING"
  trap - EXIT
  echo "Installed remote-kernels v${VERSION}" >&2
fi

exec "$BINARY" "$@"
