#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"

# Output error message - uses JSON format for session-start hook, plain text otherwise
report_error() {
    local message="$1"
    if [ "$COMMAND" = "session-start" ]; then
        echo "{\"systemMessage\": \"hive-mind: $message\"}"
        exit 0
    else
        echo "hive-mind: $message"
        exit 1
    fi
}

COMMAND="$1"

# Find bun - check standard install locations first (hooks run in non-interactive
# shells that don't source ~/.zshrc, so PATH may not include ~/.bun/bin)
BUN_PATH=""
if [ -n "$BUN_INSTALL" ] && [ -x "$BUN_INSTALL/bin/bun" ]; then
    BUN_PATH="$BUN_INSTALL/bin/bun"
elif [ -x "$HOME/.bun/bin/bun" ]; then
    BUN_PATH="$HOME/.bun/bin/bun"
elif command -v bun &> /dev/null; then
    BUN_PATH="$(command -v bun)"
fi

if [ -z "$BUN_PATH" ]; then
    report_error "bun not found. Run /hive-mind:setup to install it."
fi

if [ ! -f "$PLUGIN_ROOT/cli.js" ]; then
    report_error "cli.js not found. Try reinstalling the plugin."
fi

exec "$BUN_PATH" "$PLUGIN_ROOT/cli.js" "$@"
