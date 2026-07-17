#!/bin/bash
set -euo pipefail

# If deno is available globally, nothing to do
if command -v deno >/dev/null 2>&1; then
  exit 0
fi

# Check if we already have it in the standard location
DENO_BIN="$HOME/.deno/bin/deno"
if [ -x "$DENO_BIN" ]; then
  exit 0
fi

# ANSI via JSON unicode escapes
B='\u001b[1;32m'
R='\u001b[0m'

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OS_NAME="linux" ;;
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

# Get latest version
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

if curl -fSL "$DOWNLOAD_URL" -o "$TMPFILE"; then
  if unzip -o "$TMPFILE" -d "$TMPDIR_EXTRACT" >/dev/null 2>&1; then
    chmod +x "$TMPDIR_EXTRACT/deno"
    if "$TMPDIR_EXTRACT/deno" --version >/dev/null 2>&1; then
      mv -f "$TMPDIR_EXTRACT/deno" "$DENO_BIN"
      echo "{\"systemMessage\": \"${B}autopilot:${R} deno bootstrapped, sandboxed scripting is now available\"}"
    else
      echo "{\"systemMessage\": \"${B}autopilot:${R} downloaded deno binary is corrupt, install deno manually\"}"
    fi
  else
    echo "{\"systemMessage\": \"${B}autopilot:${R} failed to extract deno, ensure unzip is installed\"}"
  fi
else
  echo "{\"systemMessage\": \"${B}autopilot:${R} failed to download deno, install deno manually\"}"
  exit 0
fi
