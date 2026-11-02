#!/bin/bash
set -euo pipefail

# alignment-hive uninstaller: the reverse of install.sh.
# Usage: curl -fsSL https://alignment-hive.com/uninstall.sh | bash
#        curl -fsSL https://alignment-hive.com/uninstall.sh | bash -s -- --yes   (no confirmation)
#
# Removes the @alignment-hive plugins (user and local scope), the alignment-hive marketplace,
# the hive CLI (its ~/.local/bin/hive link and ~/.cache/hive), and the login in ~/.alignment-hive.
# Leaves in place, and prints: the PATH line install.sh added, plugins enabled in a project's
# shared .claude/settings.json, and each project's .claude/hive state. Claude Code marks the
# removed plugins' cached files orphaned and deletes them itself once no session uses them.

# --- Configuration ---

MARKETPLACE="alignment-hive"
CLAUDE_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
case "$CLAUDE_DIR" in "~/"*) CLAUDE_DIR="$HOME/${CLAUDE_DIR#\~/}" ;; esac
KNOWN_MARKETPLACES="$CLAUDE_DIR/plugins/known_marketplaces.json"
INSTALLED_PLUGINS="$CLAUDE_DIR/plugins/installed_plugins.json"
USER_SETTINGS="$CLAUDE_DIR/settings.json"

HIVE_LINK="$HOME/.local/bin/hive"
HIVE_CACHE="$HOME/.cache/hive"            # hive binaries (v*/hive) and local-server sockets (run/)
JQ_CACHE_DIR="$HOME/.cache/alignment-hive" # jq that install.sh downloads when none is installed
AUTH_DIR="$HOME/.alignment-hive"
LEGACY_AUTH_DIR="$HOME/.claude/hive-mind"   # login of the old hive-mind plugin

AUTH_FILE=""
case "${ALIGNMENT_HIVE_AUTH_FILE:-}" in
  "") ;;
  "~/"*) AUTH_FILE="$HOME/${ALIGNMENT_HIVE_AUTH_FILE#\~/}" ;;
  *)     AUTH_FILE="$ALIGNMENT_HIVE_AUTH_FILE" ;;
esac
# A mistyped override can name anything; only ever remove a plain file through it.
if [ -n "$AUTH_FILE" ] && [ ! -f "$AUTH_FILE" ]; then
  AUTH_FILE=""
fi

ASSUME_YES=false
JQ=""
TMP_DIR=""

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
item()    { echo "    - $1"; }

# Paths under $HOME are shown as ~/...
pretty() {
  case "$1" in
    "$HOME") echo "~" ;;
    "$HOME"/*) echo "~/${1#"$HOME"/}" ;;
    *) echo "$1" ;;
  esac
}

trap 'error "Uninstall failed at line $LINENO. Re-run with: bash -x <(curl -fsSL https://alignment-hive.com/uninstall.sh)"' ERR
trap '[ -z "$TMP_DIR" ] || rm -rf "$TMP_DIR"' EXIT

parse_args() {
  for arg in "$@"; do
    case "$arg" in
      -y|--yes) ASSUME_YES=true ;;
      -h|--help)
        echo "Usage: curl -fsSL https://alignment-hive.com/uninstall.sh | bash [-s -- --yes]"
        exit 0
        ;;
      *) error "Unknown option: $arg"; exit 1 ;;
    esac
  done
}

# --- jq, only needed to read Claude Code's plugin records ---
# Prefers an installed jq, then the one install.sh cached; otherwise downloads one into a
# temporary directory that is removed on exit.

ensure_jq() {
  if command -v jq >/dev/null 2>&1; then JQ="jq"; return 0; fi
  if [ -x "$JQ_CACHE_DIR/jq" ]; then JQ="$JQ_CACHE_DIR/jq"; return 0; fi

  local os arch os_name arch_name
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)
  case "$os" in
    linux)  os_name="linux" ;;
    darwin) os_name="macos" ;;
    *)      return 1 ;;
  esac
  case "$arch" in
    x86_64)        arch_name="amd64" ;;
    aarch64|arm64) arch_name="arm64" ;;
    *)             return 1 ;;
  esac

  TMP_DIR=$(mktemp -d)
  local url="https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-${os_name}-${arch_name}"
  curl -fsSL "$url" -o "$TMP_DIR/jq" 2>/dev/null && chmod +x "$TMP_DIR/jq" &&
    "$TMP_DIR/jq" --version >/dev/null 2>&1 || return 1
  JQ="$TMP_DIR/jq"
}

# --- Survey: what is installed ---

PLUGIN_INSTALLS=""   # plugin<TAB>scope<TAB>projectPath, one install per line
MARKETPLACE_PRESENT=false
LINK_IS_OURS=false

survey() {
  if [ -f "$INSTALLED_PLUGINS" ]; then
    if ensure_jq; then
      PLUGIN_INSTALLS=$($JQ -r --arg m "@$MARKETPLACE" '
        .plugins // {} | to_entries[] | select(.key | endswith($m))
        | .key as $plugin | .value[] | [$plugin, .scope, .projectPath // ""] | @tsv
      ' "$INSTALLED_PLUGINS" 2>/dev/null || true)
    elif grep -q "@$MARKETPLACE\"" "$INSTALLED_PLUGINS"; then
      warn "Could not get jq to read $(pretty "$INSTALLED_PLUGINS"); remove the @$MARKETPLACE plugins with /plugin in Claude Code."
    fi
  fi

  if [ -f "$KNOWN_MARKETPLACES" ] && grep -q "\"$MARKETPLACE\"" "$KNOWN_MARKETPLACES"; then
    MARKETPLACE_PRESENT=true
  elif [ -f "$USER_SETTINGS" ] && grep -q "\"$MARKETPLACE\"" "$USER_SETTINGS"; then
    MARKETPLACE_PRESENT=true
  fi

  if [ -L "$HIVE_LINK" ]; then
    case "$(readlink "$HIVE_LINK")" in
      "$HIVE_CACHE"/*) LINK_IS_OURS=true ;;
    esac
  fi
}

has_plugin() { printf '%s\n' "$PLUGIN_INSTALLS" | cut -f1 | grep -q "^$1"; }

# --- Before removing anything ---

print_plan() {
  echo ""
  info "alignment-hive uninstaller"
  echo ""
  info "Uploaded sessions are not deleted by this script."
  echo "    Uninstalling stops uploads from this machine, but projects stay shared in your"
  echo "    alignment-hive account. To stop sharing first, run 'hive consent disable' in each"
  echo "    shared project, or turn projects off at https://alignment-hive.com/consent/projects"
  echo "    (cancel below, since this removes the hive CLI). To have uploaded sessions deleted,"
  echo "    email yoav.tzfati@gmail.com."
  echo ""

  if has_plugin "model-router"; then
    warn "model-router is installed: its gateway service and the settings that route Claude Code"
    warn "through it outlive the plugin. To remove those too, cancel and ask Claude Code to"
    warn "uninstall model-router (its setup skill does it), then re-run this script."
    echo ""
  fi
  if has_plugin "remote-kernels"; then
    warn "remote-kernels is installed: stop or terminate any cloud machines it started first,"
    warn "or they keep billing your provider account."
    echo ""
  fi

  info "This will remove:"
  local plugin scope dir
  while IFS=$'\t' read -r plugin scope dir; do
    [ -n "$plugin" ] || continue
    case "$scope" in
      user)    item "plugin $plugin" ;;
      local)   item "plugin $plugin (local scope, $(pretty "$dir"))" ;;
    esac
  done <<< "$PLUGIN_INSTALLS"
  [ "$MARKETPLACE_PRESENT" = false ] || item "the $MARKETPLACE plugin marketplace"
  [ "$LINK_IS_OURS" = false ] || item "$(pretty "$HIVE_LINK") (the hive CLI)"
  local path
  for path in "$HIVE_CACHE" "$JQ_CACHE_DIR" "$AUTH_DIR" "$LEGACY_AUTH_DIR" "$AUTH_FILE"; do
    [ -z "$path" ] || [ ! -e "$path" ] || item "$(pretty "$path")"
  done
  echo ""
}

nothing_to_do() {
  [ -z "$(printf '%s\n' "$PLUGIN_INSTALLS" | awk -F'\t' '$2 != "project"')" ] &&
    [ "$MARKETPLACE_PRESENT" = false ] && [ "$LINK_IS_OURS" = false ] &&
    [ ! -e "$HIVE_CACHE" ] && [ ! -e "$JQ_CACHE_DIR" ] && [ ! -e "$AUTH_DIR" ] &&
    [ ! -e "$LEGACY_AUTH_DIR" ] && { [ -z "$AUTH_FILE" ] || [ ! -e "$AUTH_FILE" ]; }
}

confirm() {
  [ "$ASSUME_YES" = false ] || return 0
  printf "  Continue? [y/N] "
  local reply=""
  # stdin is the script itself under curl | bash, so the answer comes from the terminal.
  if ! read -r reply 2>/dev/null </dev/tty; then
    echo ""
    error "No terminal to confirm on. Re-run with: curl -fsSL https://alignment-hive.com/uninstall.sh | bash -s -- --yes"
    exit 1
  fi
  case "$reply" in
    [Yy]*) echo "" ;;
    *) info "Cancelled; nothing was removed."; exit 0 ;;
  esac
}

# --- Removal ---

uninstall_plugins() {
  [ -n "$PLUGIN_INSTALLS" ] || return 0
  if ! command -v claude >/dev/null 2>&1; then
    warn "Claude Code (claude) not found: skipping plugin removal."
    return 0
  fi

  local plugin scope dir
  while IFS=$'\t' read -r plugin scope dir; do
    case "$scope" in
      user)
        info "Uninstalling $plugin..."
        (cd "$HOME" && claude plugin uninstall "$plugin" --scope user </dev/null) ||
          warn "Failed to uninstall $plugin; remove it with /plugin in Claude Code."
        ;;
      local)
        if [ ! -d "$dir" ]; then
          warn "Skipping $plugin (local scope): $(pretty "$dir") no longer exists."
          continue
        fi
        info "Uninstalling $plugin from $(pretty "$dir")..."
        (cd "$dir" && claude plugin uninstall "$plugin" --scope local </dev/null) ||
          warn "Failed to uninstall $plugin from $(pretty "$dir")."
        ;;
    esac
  done <<< "$PLUGIN_INSTALLS"
}

remove_marketplace() {
  [ "$MARKETPLACE_PRESENT" = true ] || return 0
  if ! command -v claude >/dev/null 2>&1; then
    warn "Claude Code (claude) not found: skipping marketplace removal."
    return 0
  fi
  info "Removing the $MARKETPLACE marketplace..."
  (cd "$HOME" && claude plugin marketplace remove "$MARKETPLACE" </dev/null) ||
    warn "Failed to remove the marketplace; remove it with /plugin > Marketplaces in Claude Code."
}

remove_files() {
  if [ "$LINK_IS_OURS" = true ]; then
    rm -f "$HIVE_LINK"
    success "Removed $(pretty "$HIVE_LINK")"
  fi
  local path
  for path in "$HIVE_CACHE" "$JQ_CACHE_DIR" "$AUTH_DIR" "$LEGACY_AUTH_DIR"; do
    if [ -e "$path" ]; then
      rm -rf "$path"
      success "Removed $(pretty "$path")"
    fi
  done
  if [ -n "$AUTH_FILE" ]; then
    rm -f "$AUTH_FILE"
    success "Removed $(pretty "$AUTH_FILE")"
  fi
}

# --- Afterwards: what stays, and where ---

# Each project's .claude/hive (and the older .claude/hive-mind) sits in its main checkout. The
# projects are the ones Claude Code has transcripts for, plus any with a project-scope install.
project_state_dirs() {
  local seen="" root d f cwd sub
  {
    printf '%s\n' "$PLUGIN_INSTALLS" | cut -f3
    for d in "$CLAUDE_DIR"/projects/*/; do
      for f in "$d"*.jsonl; do
        [ -f "$f" ] || continue
        cwd=$(grep -m1 -o '"cwd":"[^"]*"' "$f" 2>/dev/null | head -1 || true)
        cwd="${cwd#\"cwd\":\"}"
        echo "${cwd%\"}"
        break
      done
    done
  } | while IFS= read -r root; do
    root="${root%%/.claude/worktrees/*}"
    [ -n "$root" ] || continue
    for sub in hive hive-mind; do
      [ -d "$root/.claude/$sub" ] || continue
      case "$seen" in *$'\n'"$root/.claude/$sub"$'\n'*) continue ;; esac
      seen="$seen"$'\n'"$root/.claude/$sub"$'\n'
      echo "$root/.claude/$sub"
    done
  done
}

print_left_in_place() {
  local lines="" plugin scope dir rc
  # Removing the marketplace drops these installs too, but not the shared settings entry.
  while IFS=$'\t' read -r plugin scope dir; do
    [ "$scope" = project ] || continue
    lines="$lines    - the \"$plugin\" entry under enabledPlugins in $(pretty "$dir")/.claude/settings.json"$'\n'
    lines="$lines      (shared with collaborators; delete it if no one there uses the plugin)"$'\n'
  done <<< "$PLUGIN_INSTALLS"

  if [ -e "$HIVE_LINK" ] || [ -L "$HIVE_LINK" ]; then
    lines="$lines    - $(pretty "$HIVE_LINK"), which doesn't point into $(pretty "$HIVE_CACHE")"$'\n'
  fi

  # install.sh appends this exact line when ~/.local/bin isn't on PATH; other tools may rely on it.
  local path_line='export PATH="$HOME/.local/bin:$PATH"'
  for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.config/fish/config.fish" "$HOME/.profile"; do
    [ -f "$rc" ] || continue
    local n
    for n in $(grep -nxF "$path_line" "$rc" 2>/dev/null | cut -d: -f1); do
      lines="$lines    - the ~/.local/bin PATH line in $(pretty "$rc") (line $n)"$'\n'
    done
  done

  local state
  while IFS= read -r state; do
    [ -n "$state" ] || continue
    lines="$lines    - $(pretty "$state") (project sharing state; delete with: rm -rf $(pretty "$state"))"$'\n'
  done < <(project_state_dirs)

  if [ -n "$lines" ]; then
    info "Left in place:"
    printf '%s' "$lines"
    echo ""
  fi
}

# --- Main ---

main() {
  parse_args "$@"
  survey

  if nothing_to_do; then
    echo ""
    info "alignment-hive is not installed for $(pretty "$CLAUDE_DIR"); nothing to remove."
    echo ""
    print_left_in_place
    exit 0
  fi

  print_plan
  confirm

  uninstall_plugins
  remove_marketplace
  remove_files

  echo ""
  success "alignment-hive uninstalled. Restart open Claude Code sessions to drop its plugins."
  echo ""
  print_left_in_place
}

main "$@"
