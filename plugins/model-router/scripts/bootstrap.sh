#!/bin/bash
set -euo pipefail

# Bootstrap script for the model-router gateway: resolves the pinned binary
# (downloading it on first use) and execs it with all arguments.
#
# `bootstrap.sh prefetch` stops after ensuring the binary is on disk instead
# of exec'ing it. `service refresh` runs the new plugin's bootstrap this way
# before switching the launcher, so a not-yet-published release aborts the
# refresh while the current service keeps running.
#
# Runs from two locations with identical behavior: the plugin's scripts/ dir
# (binary-version in the parent dir) and the service launcher copy in the
# state dir (binary-version next to this script). `model-router service`
# maintains the launcher copy; OS service units exec it so they never point
# at ephemeral per-version plugin directories.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$SCRIPT_DIR/binary-version" ]; then
  VERSION_FILE="$SCRIPT_DIR/binary-version"
else
  VERSION_FILE="$(dirname "$SCRIPT_DIR")/binary-version"
fi
VERSION=$(tr -d '[:space:]' < "$VERSION_FILE")
if [ -z "$VERSION" ]; then
  echo "Failed to read version from $VERSION_FILE" >&2
  exit 1
fi

# Let `model-router service install/refresh` find the files to copy into the
# stable launcher dir without needing an explicit --plugin-root.
export MODEL_ROUTER_BOOTSTRAP_SCRIPT="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"
export MODEL_ROUTER_VERSION_FILE="$VERSION_FILE"

# Dev mode: point MODEL_ROUTER_DEV at a locally built executable. Prefetch
# deliberately ignores it: refresh inherits the variable from the invoking
# shell, but the OS service it is about to restart never sees it, so the
# prefetch must validate the pinned release download the service will perform.
if [ "${1:-}" != "prefetch" ] && [ -n "${MODEL_ROUTER_DEV:-}" ] && [ -x "$MODEL_ROUTER_DEV" ]; then
  exec "$MODEL_ROUTER_DEV" "$@"
fi

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$OS" in
  linux) OS_TRIPLE="unknown-linux-gnu" ;;
  darwin) OS_TRIPLE="apple-darwin" ;;
  *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac
case "$ARCH" in
  x86_64) ARCH_TRIPLE="x86_64" ;;
  aarch64|arm64) ARCH_TRIPLE="aarch64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET="${ARCH_TRIPLE}-${OS_TRIPLE}"
CACHE_DIR="$HOME/.cache/model-router/v${VERSION}"
BINARY="$CACHE_DIR/model-router"
if [ ! -x "$BINARY" ]; then
  # Download and extract into a private staging dir, then atomically rename
  # the binary into place: a concurrent run or a mid-download kill (e.g. the
  # SessionStart hook timeout) can never leave a corrupt executable at
  # $BINARY, which is only ever a complete file or absent.
  ARCHIVE_NAME="model-router-${TARGET}.tar.xz"
  DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/model-router-v${VERSION}/${ARCHIVE_NAME}"
  echo "Downloading model-router v${VERSION} for ${TARGET}..." >&2
  mkdir -p "$CACHE_DIR"
  # Reap staging dirs orphaned by killed runs; only clearly-stale ones so a
  # concurrent download is never disturbed.
  find "$CACHE_DIR" -maxdepth 1 -name 'staging.*' -mmin +60 -exec rm -rf {} + 2>/dev/null || true
  STAGING=$(mktemp -d "${CACHE_DIR}/staging.XXXXXX")
  trap 'rm -rf "$STAGING"' EXIT
  if ! curl -fSL "$DOWNLOAD_URL" -o "$STAGING/$ARCHIVE_NAME" 2>/dev/null; then
    echo "Failed to download from: $DOWNLOAD_URL" >&2
    echo "If this version was just published, the release may still be building — retry in a few minutes." >&2
    echo "(For local development, point MODEL_ROUTER_DEV at a built binary.)" >&2
    exit 1
  fi
  tar -xf "$STAGING/$ARCHIVE_NAME" -C "$STAGING"
  FOUND=$(find "$STAGING" -name "model-router" -type f 2>/dev/null | head -1)
  if [ -z "$FOUND" ]; then
    echo "Binary not found in archive" >&2
    exit 1
  fi
  chmod +x "$FOUND"
  mv -f "$FOUND" "$BINARY"
  rm -rf "$STAGING"
  trap - EXIT
fi

if [ "${1:-}" = "prefetch" ]; then
  exit 0
fi

exec "$BINARY" "$@"
