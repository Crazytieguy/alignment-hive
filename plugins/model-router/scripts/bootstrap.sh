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
#
# The platform-specific marketplace entries ship the binary inside the plugin
# zip, at bin/model-router-<target>.tar.xz. When it is there the binary comes
# from the plugin itself, so plugin and binary are never out of step. The
# plain (path-source) plugin has no bin/ and downloads as before. A bin/ that
# holds some other platform's binary is an error, not a fallback.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$SCRIPT_DIR/binary-version" ]; then
  # Launcher copy: no plugin around it, so BUNDLE_DIR is deliberately a
  # directory that does not exist and the download path is always taken.
  VERSION_FILE="$SCRIPT_DIR/binary-version"
  BUNDLE_DIR="$SCRIPT_DIR/bin"
else
  VERSION_FILE="$(dirname "$SCRIPT_DIR")/binary-version"
  BUNDLE_DIR="$(dirname "$SCRIPT_DIR")/bin"
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
case "${1:-}" in
  prefetch | platform-check) ;;
  *)
    if [ -n "${MODEL_ROUTER_DEV:-}" ] && [ -x "$MODEL_ROUTER_DEV" ]; then
      exec "$MODEL_ROUTER_DEV" "$@"
    fi
    ;;
esac

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

# A platform-specific plugin zip carries exactly one target's binary. A bin/
# without ours means the wrong variant is installed: stop, rather than quietly
# downloading, which would hand back the plugin/binary skew the bundle exists
# to remove. `platform-check` runs just this test — no download, no exec — and
# prints the entry that should have been installed, for the setup skill.
if [ -d "$BUNDLE_DIR" ] && [ ! -f "$BUNDLE_DIR/model-router-${TARGET}.tar.xz" ]; then
  if [ "${1:-}" = "platform-check" ]; then
    echo "model-router-${TARGET}"
  fi
  echo "model-router: wrong platform, run /model-router:setup to fix" >&2
  exit 1
fi
if [ "${1:-}" = "platform-check" ]; then
  exit 0
fi

CACHE_DIR="$HOME/.cache/model-router/v${VERSION}"
BINARY="$CACHE_DIR/model-router"
if [ ! -x "$BINARY" ]; then
  # Download and extract into a private staging dir, then atomically rename
  # the binary into place: a concurrent run or a mid-download kill (e.g. the
  # SessionStart hook timeout) can never leave a corrupt executable at
  # $BINARY, which is only ever a complete file or absent.
  ARCHIVE_NAME="model-router-${TARGET}.tar.xz"
  # Present only in the platform-specific plugin zips; a mismatched one already
  # exited above, so reaching here with no bundle means the plain plugin.
  ARCHIVE="$BUNDLE_DIR/$ARCHIVE_NAME"
  mkdir -p "$CACHE_DIR"
  # Reap staging dirs orphaned by killed runs; only clearly-stale ones so a
  # concurrent download is never disturbed.
  find "$CACHE_DIR" -maxdepth 1 -name 'staging.*' -mmin +60 -exec rm -rf {} + 2>/dev/null || true
  STAGING=$(mktemp -d "${CACHE_DIR}/staging.XXXXXX")
  trap 'rm -rf "$STAGING"' EXIT
  if [ ! -f "$ARCHIVE" ]; then
    ARCHIVE="$STAGING/$ARCHIVE_NAME"
    DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/model-router-v${VERSION}/${ARCHIVE_NAME}"
    echo "Downloading model-router v${VERSION} for ${TARGET}..." >&2
    if ! curl -fSL "$DOWNLOAD_URL" -o "$ARCHIVE" 2>/dev/null; then
      echo "Failed to download from: $DOWNLOAD_URL" >&2
      echo "If this version was just published, the release may still be building — retry in a few minutes." >&2
      echo "(For local development, point MODEL_ROUTER_DEV at a built binary.)" >&2
      exit 1
    fi
  fi
  tar -xf "$ARCHIVE" -C "$STAGING"
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
