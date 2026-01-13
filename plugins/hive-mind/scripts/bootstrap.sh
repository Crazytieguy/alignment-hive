#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"

if ! command -v bun &> /dev/null; then
    if [ "$1" = "session-start" ]; then
        echo '{"systemMessage": "To run hive-mind: curl -fsSL https://bun.sh/install | bash"}'
        exit 0
    else
        echo "To run hive-mind, install Bun: curl -fsSL https://bun.sh/install | bash"
        exit 1
    fi
fi

exec bun "$PLUGIN_ROOT/cli.js" "$@"
