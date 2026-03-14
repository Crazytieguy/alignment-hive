#!/bin/bash
set -euo pipefail

trap 'error "Install failed at line $LINENO. Re-run with: bash -x <(curl -fsSL https://alignment-hive.com/install.sh)"' ERR

# alignment-hive installer
# Usage: curl -fsSL https://alignment-hive.com/install.sh | bash

# --- Configuration ---

CLIENT_ID="${ALIGNMENT_HIVE_CLIENT_ID:-client_01KE10CZ6FFQB9TR2NVBQJ4AKV}"
WORKOS_API="https://api.workos.com/user_management"
AUTH_DIR="$HOME/.alignment-hive"

resolve_auth_file() {
  local env_path="${ALIGNMENT_HIVE_AUTH_FILE:-}"
  if [ -n "$env_path" ]; then
    case "$env_path" in
      "~/"*) echo "$HOME/${env_path#\~/}" ;;
      *)     echo "$env_path" ;;
    esac
  else
    echo "$AUTH_DIR/auth.json"
  fi
}

AUTH_FILE="$(resolve_auth_file)"
JQ_CACHE_DIR="$HOME/.cache/alignment-hive"
JQ_BINARY="$JQ_CACHE_DIR/jq"
KNOWN_MARKETPLACES="$HOME/.claude/plugins/known_marketplaces.json"
INSTALLED_PLUGINS="$HOME/.claude/plugins/installed_plugins.json"
JQ=""

# --- Output helpers ---

BOLD=""
GREEN=""
YELLOW=""
RED=""
RESET=""

if [ -t 1 ] || [ -t 2 ]; then
  BOLD=$(tput bold 2>/dev/null || true)
  GREEN=$(tput setaf 2 2>/dev/null || true)
  YELLOW=$(tput setaf 3 2>/dev/null || true)
  RED=$(tput setaf 1 2>/dev/null || true)
  RESET=$(tput sgr0 2>/dev/null || true)
fi

info()    { echo "  ${BOLD}$1${RESET}"; }
success() { echo "  ${GREEN}$1${RESET}"; }
warn()    { echo "  ${YELLOW}$1${RESET}"; }
error()   { echo "  ${RED}$1${RESET}" >&2; }

# --- Step 1: Check claude CLI ---

check_claude() {
  if ! command -v claude >/dev/null 2>&1; then
    error "Claude Code is not installed."
    echo "  Install it first: curl -fsSL https://claude.ai/install.sh | bash"
    exit 1
  fi
}

# --- Step 2: Bootstrap jq ---
# Adapted from plugins/autopilot/scripts/ensure-jq.sh

ensure_jq() {
  if command -v jq >/dev/null 2>&1; then
    JQ="jq"
    return 0
  fi

  if [ -x "$JQ_BINARY" ]; then
    JQ="$JQ_BINARY"
    return 0
  fi

  local os arch os_name arch_name
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "$os" in
    linux)  os_name="linux" ;;
    darwin) os_name="macos" ;;
    *)      error "Unsupported OS: $os"; exit 1 ;;
  esac

  case "$arch" in
    x86_64)        arch_name="amd64" ;;
    aarch64|arm64) arch_name="arm64" ;;
    *)             error "Unsupported architecture: $arch"; exit 1 ;;
  esac

  local url="https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-${os_name}-${arch_name}"

  info "Downloading jq..."
  mkdir -p "$JQ_CACHE_DIR"

  if ! curl -fSL "$url" -o "$JQ_BINARY" 2>/dev/null; then
    rm -f "$JQ_BINARY"
    error "Failed to download jq. Install jq manually and re-run."
    exit 1
  fi

  chmod +x "$JQ_BINARY"

  if ! "$JQ_BINARY" --version >/dev/null 2>&1; then
    rm -f "$JQ_BINARY"
    error "Downloaded jq binary is corrupt. Install jq manually."
    exit 1
  fi

  JQ="$JQ_BINARY"
  success "jq bootstrapped"
}

# --- Step 3: Add marketplace ---

add_marketplace() {
  if [ -f "$KNOWN_MARKETPLACES" ] && $JQ -e '."alignment-hive"' "$KNOWN_MARKETPLACES" >/dev/null 2>&1; then
    info "Marketplace already added"
    return 0
  fi

  info "Adding alignment-hive marketplace..."
  claude plugin marketplace add Crazytieguy/alignment-hive
  success "Marketplace added"
}

# --- Step 4: Enable auto-update ---

enable_auto_update() {
  if [ ! -f "$KNOWN_MARKETPLACES" ]; then
    warn "Marketplace file not found. Enable auto-update manually: /plugin > Marketplaces > alignment-hive > Enable auto-update"
    return 0
  fi

  if ! $JQ -e '."alignment-hive"' "$KNOWN_MARKETPLACES" >/dev/null 2>&1; then
    warn "alignment-hive not found in marketplaces. Enable auto-update manually."
    return 0
  fi

  if $JQ -e '."alignment-hive".autoUpdate == true' "$KNOWN_MARKETPLACES" >/dev/null 2>&1; then
    info "Auto-update already enabled"
    return 0
  fi

  info "Enabling auto-update..."
  local tmp
  tmp=$(mktemp "$HOME/.claude/plugins/known_marketplaces.XXXXXX")
  if $JQ '."alignment-hive".autoUpdate = true' "$KNOWN_MARKETPLACES" > "$tmp"; then
    mv "$tmp" "$KNOWN_MARKETPLACES"
    success "Auto-update enabled"
  else
    rm -f "$tmp"
    warn "Failed to enable auto-update. Enable manually: /plugin > Marketplaces > alignment-hive > Enable auto-update"
  fi
}

# --- Step 5: Install hive plugin ---

install_hive_plugin() {
  # Dev mode: plugin is loaded from the local repo, skip marketplace install
  if [ "${ALIGNMENT_HIVE_DEV:-}" = "1" ]; then
    info "Dev mode: skipping plugin install (using local plugin)"
    return 0
  fi

  if [ -f "$INSTALLED_PLUGINS" ] && $JQ -e '.plugins."hive@alignment-hive"' "$INSTALLED_PLUGINS" >/dev/null 2>&1; then
    info "hive plugin already installed"
    return 0
  fi

  info "Installing hive plugin..."
  claude plugin install hive@alignment-hive
  success "hive plugin installed"
}

# --- Step 6: Install hive binary globally ---

install_hive_binary() {
  # Dev mode: build from source
  if [ "${ALIGNMENT_HIVE_DEV:-}" = "1" ]; then
    # Derive repo root from script location
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local repo_root
    repo_root="$(cd "$script_dir/../../.." && pwd)"

    info "Building hive binary from source..."
    mkdir -p "$repo_root/.dev"
    if bun build --compile "$repo_root/packages/hive-cli/src/hive-cli.ts" --outfile "$repo_root/.dev/hive"; then
      export PATH="$repo_root/.dev:$PATH"
      success "Dev binary built at $repo_root/.dev/hive"
    else
      error "Failed to build hive binary from source."
      exit 1
    fi
    return 0
  fi

  # Find plugin install path to read cli-version
  if [ ! -f "$INSTALLED_PLUGINS" ]; then
    warn "Cannot find installed plugins. Skipping binary install."
    return 0
  fi

  local plugin_path
  plugin_path=$($JQ -r '.plugins."hive@alignment-hive"[0].installPath // empty' "$INSTALLED_PLUGINS" 2>/dev/null)
  if [ -z "$plugin_path" ]; then
    warn "hive plugin path not found. Skipping binary install."
    return 0
  fi

  local version_file="$plugin_path/cli-version"
  if [ ! -f "$version_file" ]; then
    warn "cli-version not found. Skipping binary install."
    return 0
  fi

  local version
  version=$(cat "$version_file" | tr -d '[:space:]')
  if [ -z "$version" ]; then
    warn "cli-version is empty. Skipping binary install."
    return 0
  fi

  # Detect platform
  local os arch os_name arch_name
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "$os" in
    linux)  os_name="linux" ;;
    darwin) os_name="darwin" ;;
    *)      error "Unsupported OS: $os"; exit 1 ;;
  esac

  case "$arch" in
    x86_64)        arch_name="x64" ;;
    aarch64|arm64) arch_name="arm64" ;;
    *)             error "Unsupported architecture: $arch"; exit 1 ;;
  esac

  local cache_dir="$HOME/.cache/hive/v${version}"
  local binary="$cache_dir/hive"
  local target="${os_name}-${arch_name}"

  # Download if not already cached
  if [ ! -x "$binary" ]; then
    local binary_name="hive-cli-${target}"
    local download_url="https://github.com/Crazytieguy/alignment-hive/releases/download/hive-cli-v${version}/${binary_name}"

    info "Downloading hive binary v${version}..."
    mkdir -p "$cache_dir"

    if ! curl -fSL "$download_url" -o "$binary" 2>/dev/null; then
      rm -f "$binary"
      error "Failed to download hive binary v${version}."
      exit 1
    fi

    chmod +x "$binary"
  fi

  # Install to ~/.local/bin
  mkdir -p "$HOME/.local/bin"
  cp "$binary" "$HOME/.local/bin/hive"

  # Ensure ~/.local/bin is in PATH
  if ! echo "$PATH" | tr ':' '\n' | grep -qx "$HOME/.local/bin"; then
    local shell_config=""
    case "${SHELL:-/bin/bash}" in
      */zsh)  shell_config="$HOME/.zshrc" ;;
      */bash) shell_config="$HOME/.bashrc" ;;
      */fish) shell_config="$HOME/.config/fish/config.fish" ;;
      *)      shell_config="$HOME/.profile" ;;
    esac

    if [ -n "$shell_config" ]; then
      local path_line='export PATH="$HOME/.local/bin:$PATH"'
      if ! grep -qF '.local/bin' "$shell_config" 2>/dev/null; then
        echo "$path_line" >> "$shell_config"
        warn "Added ~/.local/bin to PATH in $shell_config (restart your shell to activate)"
      fi
    fi
  fi

  success "hive binary installed to ~/.local/bin/hive"
}

# --- Auth helpers ---

save_auth() {
  local response="$1" authenticated_at="${2:-}"
  mkdir -p "$AUTH_DIR"
  if [ -n "$authenticated_at" ]; then
    echo "$response" | $JQ --argjson at "$authenticated_at" '. + {authenticated_at: $at}' > "$AUTH_FILE"
  else
    echo "$response" > "$AUTH_FILE"
  fi
  chmod 600 "$AUTH_FILE"
}

# --- Step 6: Check existing auth ---

check_auth() {
  if [ ! -f "$AUTH_FILE" ]; then
    return 1
  fi

  local refresh_token
  refresh_token=$($JQ -r '.refresh_token // empty' "$AUTH_FILE" 2>/dev/null)
  if [ -z "$refresh_token" ]; then
    return 1
  fi

  local response
  response=$(curl -s -X POST "$WORKOS_API/authenticate" \
    -d "grant_type=refresh_token" \
    -d "refresh_token=$refresh_token" \
    -d "client_id=$CLIENT_ID")

  if ! echo "$response" | $JQ -e '.access_token and .refresh_token and .user' >/dev/null 2>&1; then
    return 1
  fi

  local authenticated_at
  authenticated_at=$($JQ -r '.authenticated_at // empty' "$AUTH_FILE" 2>/dev/null || true)
  save_auth "$response" "$authenticated_at"

  local email
  email=$(echo "$response" | $JQ -r '.user.email // "unknown"')
  success "Authenticated as $email"
  return 0
}

# --- Step 7: Device auth flow ---
# Based on docs/workos-device-auth-reference.md

device_auth_flow() {
  local response
  response=$(curl -s -X POST "$WORKOS_API/authorize/device" \
    -d "client_id=$CLIENT_ID")

  local device_code user_code verification_uri interval expires_in
  device_code=$(echo "$response" | $JQ -r '.device_code // empty')
  user_code=$(echo "$response" | $JQ -r '.user_code // empty')
  verification_uri=$(echo "$response" | $JQ -r '.verification_uri_complete // empty')
  interval=$(echo "$response" | $JQ -r '.interval // 5')
  expires_in=$(echo "$response" | $JQ -r '.expires_in // 300')

  if [ -z "$device_code" ]; then
    error "Failed to start authentication."
    return 1
  fi

  info "Opening browser to confirm authentication..."
  echo "    $verification_uri"

  # Try to open browser
  open "$verification_uri" 2>/dev/null || xdg-open "$verification_uri" 2>/dev/null || wslview "$verification_uri" 2>/dev/null || true

  info "Confirm code ${BOLD}${GREEN}$user_code${RESET} in your browser, then wait..."

  # Poll for token
  local deadline=$((SECONDS + expires_in))
  while [ $SECONDS -lt $deadline ]; do
    sleep "$interval"

    local token_response
    token_response=$(curl -s -X POST "$WORKOS_API/authenticate" \
      -d "grant_type=urn:ietf:params:oauth:grant-type:device_code" \
      -d "device_code=$device_code" \
      -d "client_id=$CLIENT_ID")

    local err
    err=$(echo "$token_response" | $JQ -r '.error // empty')

    if [ -z "$err" ]; then
      save_auth "$token_response" "$(date +%s)000"

      local email first_name
      email=$(echo "$token_response" | $JQ -r '.user.email // "unknown"')
      first_name=$(echo "$token_response" | $JQ -r '.user.first_name // empty')

      success "Authenticated as ${first_name:-$email} ($email)"
      return 0
    elif [ "$err" = "slow_down" ]; then
      interval=$((interval + 1))
    elif [ "$err" != "authorization_pending" ]; then
      error "Authentication failed: $err"
      return 1
    fi
  done

  error "Authentication timed out."
  return 1
}

# --- Main ---

main() {
  echo ""
  info "alignment-hive installer"
  echo ""

  check_claude
  ensure_jq
  add_marketplace
  enable_auto_update
  install_hive_plugin
  install_hive_binary

  echo ""

  if check_auth; then
    : # Already authenticated
  else
    printf "  Authenticate to enable session sharing? (Requires an alignment-hive invite) [Y/n] "
    read -r REPLY < /dev/tty 2>/dev/null || REPLY="n"
    case "$REPLY" in
      [Nn]*)
        info "You can authenticate later by re-running this script."
        ;;
      *)
        echo ""
        if ! device_auth_flow; then
          warn "Authentication failed. Check your email for an alignment-hive invite,"
          warn "or contact yoav.tzfati@gmail.com to request one."
          echo ""
          info "Next steps:"
          echo "    1. Open Claude Code in a project directory"
          echo "    2. Run /hive:align"
          echo ""
          exit 0
        fi
        ;;
    esac
  fi

  echo ""

  # Consent + project setup (handled entirely by the CLI)
  if command -v hive >/dev/null 2>&1 && [ -f "$AUTH_FILE" ]; then
    if ! hive consent setup; then
      warn "Consent setup failed. You can retry later with: hive consent setup"
    fi
    echo ""
    echo "  Sessions are uploaded after a 24-hour review period."
    echo "  Run 'hive upload --help' to learn about reviewing, excluding, and snoozing uploads."
    echo ""
  fi

  info "Next steps:"
  echo "    1. Open Claude Code in a project directory"
  echo "    2. Run /hive:align"
  echo ""
}

main "$@"
