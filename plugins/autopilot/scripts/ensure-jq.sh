#!/bin/bash
set -euo pipefail

# If jq is available globally, nothing to do
if command -v jq >/dev/null 2>&1; then
  exit 0
fi

# Check if we already bootstrapped it
CACHE_DIR="$HOME/.cache/autopilot"
BINARY="$CACHE_DIR/jq"

if [ -x "$BINARY" ]; then
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
  darwin) OS_NAME="macos" ;;
  *)      echo "{\"systemMessage\": \"${B}autopilot:${R} cannot bootstrap jq, unsupported OS: $OS, install jq manually\"}"
          exit 0 ;;
esac

case "$ARCH" in
  x86_64)        ARCH_NAME="amd64" ;;
  aarch64|arm64) ARCH_NAME="arm64" ;;
  *)             echo "{\"systemMessage\": \"${B}autopilot:${R} cannot bootstrap jq, unsupported architecture: $ARCH, install jq manually\"}"
                 exit 0 ;;
esac

JQ_VERSION="1.7.1"
DOWNLOAD_URL="https://github.com/jqlang/jq/releases/download/jq-${JQ_VERSION}/jq-${OS_NAME}-${ARCH_NAME}"

mkdir -p "$CACHE_DIR"

# Download to a temp file in the destination dir (same filesystem), then
# atomically rename into place — concurrent sessions run this hook at the
# same time, and a half-written binary at $BINARY would be executed by them.
TMP_BINARY=$(mktemp "$CACHE_DIR/jq.XXXXXX")
trap 'rm -f "$TMP_BINARY"' EXIT

if curl -fSL "$DOWNLOAD_URL" -o "$TMP_BINARY"; then
  chmod 755 "$TMP_BINARY"  # mktemp creates 0600; restore the usual binary mode
  # Verify the binary works (catches corrupt/partial downloads)
  if ! "$TMP_BINARY" --version >/dev/null 2>&1; then
    echo "{\"systemMessage\": \"${B}autopilot:${R} downloaded jq binary is corrupt, install jq manually\"}"
    exit 0
  fi
  mv -f "$TMP_BINARY" "$BINARY"
  echo "{\"systemMessage\": \"${B}autopilot:${R} jq bootstrapped, auto-deny is now active\"}"
else
  echo "{\"systemMessage\": \"${B}autopilot:${R} failed to download jq, auto-deny disabled until jq is installed\"}"
  exit 0
fi
