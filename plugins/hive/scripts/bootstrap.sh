#!/bin/bash
# Bootstrap script for hive CLI binary.
# Ensures the correct version is cached and that ~/.local/bin/hive points at it, then exec's
# the binary with all arguments, so the caller can pipe stdin to it.
#
# For expected issues (not installed, download failed) it prints the plugin band's row as JSON
# ({"notices": [...]}, the Notice shape in hooks/notice-band.tsx) in place of the CLI's output.
# Unexpected errors go to stderr (caller redirects to error log).
set -euo pipefail

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

# One download at a time: sessions started together after a cli-version bump would each fetch
# the binary. The lock is a directory (mkdir is atomic). Its holder can be killed mid-download on
# the hook's 10s timeout, so a lock older than a minute is taken over. Two downloads at once are
# only wasted work: each writes its own tmpfile and moves it into place atomically.
LOCK="$CACHE_DIR/.download-lock"

acquire_lock() {
  mkdir "$LOCK" 2>/dev/null && return 0
  [ -n "$(find "$LOCK" -maxdepth 0 -mmin +1 2>/dev/null)" ] || return 1
  rmdir "$LOCK" 2>/dev/null || true
  mkdir "$LOCK" 2>/dev/null
}

if [ ! -x "$BINARY" ]; then
  mkdir -p "$CACHE_DIR"
  # Partial downloads that a kill on the hook's timeout left behind.
  find "$CACHE_BASE" -maxdepth 2 -type f -name '.hive.tmp.*' -mmin +60 -exec rm -f {} + 2>/dev/null || true

  if acquire_lock; then
    # The hook can be killed on its timeout mid-download; never leave a partial file behind.
    TMPFILE="$CACHE_DIR/.hive.tmp.$$"
    trap 'rm -f "$TMPFILE"; rmdir "$LOCK" 2>/dev/null || true' EXIT
    # Another start may have finished the download while this one waited for the lock.
    if [ ! -x "$BINARY" ]; then
      BINARY_NAME="hive-cli-${TARGET}"
      DOWNLOAD_URL="https://github.com/Crazytieguy/alignment-hive/releases/download/hive-cli-v${VERSION}/${BINARY_NAME}"

      echo "Downloading hive-cli v${VERSION} for ${TARGET}..." >&2
      if ! curl -fsSL "$DOWNLOAD_URL" -o "$TMPFILE"; then
        echo "Failed to download hive-cli v${VERSION} from $DOWNLOAD_URL" >&2
        install_notice update-failed problem "CLI update failed"
        exit 0
      fi

      chmod +x "$TMPFILE"
      mv "$TMPFILE" "$BINARY"
      echo "Installed hive-cli v${VERSION}" >&2
    fi
    # Released here, not only on exit: the exec below runs no EXIT trap.
    rmdir "$LOCK" 2>/dev/null || true
    trap - EXIT
  else
    # Another start is downloading: wait a few seconds for its binary, else leave it to a later call.
    waited=0
    while [ ! -x "$BINARY" ] && [ "$waited" -lt 25 ]; do
      sleep 0.2
      waited=$((waited + 1))
    done
    if [ ! -x "$BINARY" ]; then
      printf '{"notices": [{"id": "update-pending", "severity": "info", "text": "CLI update downloading"}]}\n'
      exit 0
    fi
  fi
fi

# Whether version $1 is older than $2, both X.Y.Z; false when either isn't.
older() {
  [[ "$1" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || return 1
  local a=$((10#${BASH_REMATCH[1]} * 1000000 + 10#${BASH_REMATCH[2]} * 1000 + 10#${BASH_REMATCH[3]}))
  [[ "$2" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || return 1
  local b=$((10#${BASH_REMATCH[1]} * 1000000 + 10#${BASH_REMATCH[2]} * 1000 + 10#${BASH_REMATCH[3]}))
  (( a < b ))
}

# Every start, not only after a download: the symlink must track this plugin's cli-version,
# not whichever version was downloaded last. It never moves back to an older version, though,
# as a session still running an older plugin would have it: the retrieval and debrief skills
# run the `hive` on the PATH, and older CLIs can number a transcript's entries differently.
# So a release that pins an older CLI to roll back leaves the PATH on the newer one. A link
# already at this version is left as it is.
LINKED=$(readlink "$HOME/.local/bin/hive" 2>/dev/null || true)
if [ "$LINKED" != "$BINARY" ] && ! { [ -x "$LINKED" ] && [[ "$LINKED" == "$CACHE_BASE"/v*/hive ]] &&
  [[ "$LINKED" =~ /v([^/]+)/hive$ ]] && older "$VERSION" "${BASH_REMATCH[1]}"; }; then
  mkdir -p "$HOME/.local/bin"
  ln -sf "$BINARY" "$HOME/.local/bin/hive"
fi

exec "$BINARY" "$@"
