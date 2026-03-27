#!/bin/bash
set -euo pipefail

# Bootstrap script for hive CLI binary.
# Ensures the correct version is cached and updates ~/.local/bin/hive.
# exec's the binary with all arguments, so the caller can pipe stdin to it.
#
# Outputs JSON systemMessage to stdout for expected issues (not installed, download failed).
# Unexpected errors go to stderr (caller redirects to error log).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
CACHE_BASE="$HOME/.cache/hive"

# --- Check if hive is installed globally ---

if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  echo '{"systemMessage": "\u001b[1mhive:\u001b[0m to install, run \u001b[1;35m$ curl -fsSL https://alignment-hive.com/install.sh | bash\u001b[0m"}'
  exit 0
fi

# --- Read expected version from cli-version file ---

CLI_VERSION_FILE="$PLUGIN_ROOT/cli-version"
if [ ! -f "$CLI_VERSION_FILE" ]; then
  echo "cli-version file not found at $CLI_VERSION_FILE" >&2
  exit 1
fi
VERSION=$(tr -d '[:space:]' < "$CLI_VERSION_FILE")
if [ -z "$VERSION" ]; then
  echo "cli-version file is empty" >&2
  exit 1
fi

# --- Detect platform ---

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

# --- Ensure correct version is cached ---

CACHE_DIR="$CACHE_BASE/v${VERSION}"
BINARY="$CACHE_DIR/hive"

if [ ! -x "$BINARY" ]; then
  BINARY_NAME="hive-cli-${TARGET}"
  DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/hive-cli-v${VERSION}/${BINARY_NAME}"

  echo "Downloading hive-cli v${VERSION} for ${TARGET}..." >&2
  mkdir -p "$CACHE_DIR"

  TMPFILE="$CACHE_DIR/.hive.tmp.$$"
  if ! curl -fSL "$DOWNLOAD_URL" -o "$TMPFILE"; then
    echo "Failed to download hive-cli v${VERSION} from $DOWNLOAD_URL" >&2
    rm -f "$TMPFILE"
    echo '{"systemMessage": "\u001b[1mhive:\u001b[0m CLI update failed"}'
    exit 0
  fi

  chmod +x "$TMPFILE"
  mv "$TMPFILE" "$BINARY"

  mkdir -p "$HOME/.local/bin"
  ln -sf "$BINARY" "$HOME/.local/bin/hive"

  echo "Installed hive-cli v${VERSION}" >&2
fi

exec "$BINARY" "$@"
