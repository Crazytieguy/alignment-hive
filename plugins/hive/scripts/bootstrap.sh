#!/bin/bash
set -euo pipefail

# Bootstrap script for hive CLI binary.
# Ensures the correct version is cached and that ~/.local/bin/hive points at it, then exec's
# the binary with all arguments, so the caller can pipe stdin to it.
#
# For expected issues (not installed, download failed) it prints the plugin band's row as JSON
# ({"notices": [...]}, the Notice shape in hooks/notice-band.tsx) in place of the CLI's output.
# Unexpected errors go to stderr (caller redirects to error log).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
CACHE_BASE="$HOME/.cache/hive"

# A row whose button copies the install script's command (it asks questions, so it runs in a
# terminal of the person's own): id, severity, text.
install_notice() {
  printf '{"notices": [{"id": "%s", "severity": "%s", "text": "%s", "actions": [{"id": "install", "label": "Copy install command", "kind": "copy", "command": "curl -fsSL https://alignment-hive.com/install.sh | bash"}]}]}\n' "$1" "$2" "$3"
}

# --- Check if hive is installed globally ---

if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  install_notice install action "CLI not installed"
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
  linux|darwin) ;;
  *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
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

  # The hook can be killed on its timeout mid-download; never leave a partial file behind.
  TMPFILE="$CACHE_DIR/.hive.tmp.$$"
  trap 'rm -f "$TMPFILE"' EXIT
  if ! curl -fsSL "$DOWNLOAD_URL" -o "$TMPFILE"; then
    echo "Failed to download hive-cli v${VERSION} from $DOWNLOAD_URL" >&2
    install_notice update-failed problem "CLI update failed"
    exit 0
  fi

  chmod +x "$TMPFILE"
  mv "$TMPFILE" "$BINARY"
  trap - EXIT

  echo "Installed hive-cli v${VERSION}" >&2
fi

# Every start, not only after a download: the symlink must track this plugin's cli-version,
# not whichever version was downloaded last.
mkdir -p "$HOME/.local/bin"
ln -sf "$BINARY" "$HOME/.local/bin/hive"

exec "$BINARY" "$@"
