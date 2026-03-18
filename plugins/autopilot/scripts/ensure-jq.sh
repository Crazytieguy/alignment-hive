#!/bin/bash
set -euo pipefail

# If jq is available globally, nothing to do
if command -v jq >/dev/null 2>&1; then
  exit 0
fi

# Check if we already bootstrapped it
CACHE_DIR="${CLAUDE_PLUGIN_DATA:-$HOME/.cache/autopilot}"
BINARY="$CACHE_DIR/jq"

if [ -x "$BINARY" ]; then
  exit 0
fi

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OS_NAME="linux" ;;
  darwin) OS_NAME="macos" ;;
  *)      echo "{\"systemMessage\": \"autopilot: Cannot bootstrap jq — unsupported OS: $OS. Install jq manually.\"}"
          exit 0 ;;
esac

case "$ARCH" in
  x86_64)        ARCH_NAME="amd64" ;;
  aarch64|arm64) ARCH_NAME="arm64" ;;
  *)             echo "{\"systemMessage\": \"autopilot: Cannot bootstrap jq — unsupported architecture: $ARCH. Install jq manually.\"}"
                 exit 0 ;;
esac

JQ_VERSION="1.7.1"
DOWNLOAD_URL="https://github.com/jqlang/jq/releases/download/jq-${JQ_VERSION}/jq-${OS_NAME}-${ARCH_NAME}"

mkdir -p "$CACHE_DIR"

if curl -fSL "$DOWNLOAD_URL" -o "$BINARY"; then
  chmod +x "$BINARY"
  # Verify the binary works (catches corrupt/partial downloads)
  if ! "$BINARY" --version >/dev/null 2>&1; then
    rm -f "$BINARY"
    echo '{"systemMessage": "autopilot: Downloaded jq binary is corrupt. Install jq manually."}'
    exit 0
  fi
  echo '{"systemMessage": "autopilot: jq bootstrapped. Auto-deny is now active."}'
else
  rm -f "$BINARY"
  echo '{"systemMessage": "autopilot: Failed to download jq. Auto-deny is disabled until jq is installed."}'
  exit 0
fi
