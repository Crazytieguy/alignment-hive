#!/bin/bash
set -euo pipefail

# Runs on every session start regardless of sandbox state, so deno is already
# there in the first session after the sandbox is enabled.
# shellcheck source=find-deno.sh
if source "$(dirname "${BASH_SOURCE[0]}")/find-deno.sh"; then
  exit 0
fi
DENO_BIN="$HOME/.deno/bin/deno"

# ANSI via JSON unicode escapes
B='\u001b[1;32m'
R='\u001b[0m'

# Release assets are named by full target triple (deno-x86_64-unknown-linux-gnu.zip)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OS_NAME="unknown-linux-gnu" ;;
  darwin) OS_NAME="apple-darwin" ;;
  *)      echo "{\"systemMessage\": \"${B}autopilot:${R} cannot bootstrap deno, unsupported OS: $OS, install deno manually\"}"
          exit 0 ;;
esac

case "$ARCH" in
  x86_64)        ARCH_NAME="x86_64" ;;
  aarch64|arm64) ARCH_NAME="aarch64" ;;
  *)             echo "{\"systemMessage\": \"${B}autopilot:${R} cannot bootstrap deno, unsupported architecture: $ARCH, install deno manually\"}"
                 exit 0 ;;
esac

DENO_VERSION=$(curl -fSs https://dl.deno.land/release-latest.txt 2>/dev/null || echo "")
if [ -z "$DENO_VERSION" ]; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} failed to fetch deno version, install deno manually\"}"
  exit 0
fi

DOWNLOAD_URL="https://dl.deno.land/release/${DENO_VERSION}/deno-${ARCH_NAME}-${OS_NAME}.zip"

mkdir -p "$HOME/.deno/bin"

# Download and extract into a temp dir under $HOME/.deno (same filesystem as
# the final path — /tmp may not be, where mv degrades to a non-atomic copy),
# then atomically rename into place — concurrent sessions run this hook at the
# same time, and a half-written binary at $DENO_BIN would be executed by them.
TMPDIR_EXTRACT=$(mktemp -d "$HOME/.deno/extract-XXXXXXXX")
trap 'rm -rf "$TMPDIR_EXTRACT"' EXIT
TMPFILE="$TMPDIR_EXTRACT/deno.zip"

if ! curl -fSL "$DOWNLOAD_URL" -o "$TMPFILE"; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} failed to download deno, install deno manually\"}"
  exit 0
fi
if ! unzip -o "$TMPFILE" -d "$TMPDIR_EXTRACT" >/dev/null 2>&1; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} failed to extract deno, ensure unzip is installed\"}"
  exit 0
fi
chmod +x "$TMPDIR_EXTRACT/deno"
if ! "$TMPDIR_EXTRACT/deno" --version >/dev/null 2>&1; then
  echo "{\"systemMessage\": \"${B}autopilot:${R} downloaded deno binary is corrupt, install deno manually\"}"
  exit 0
fi
mv -f "$TMPDIR_EXTRACT/deno" "$DENO_BIN"
echo "{\"systemMessage\": \"${B}autopilot:${R} deno bootstrapped, sandboxed scripting is now available\"}"
