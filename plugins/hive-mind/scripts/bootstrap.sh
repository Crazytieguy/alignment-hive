#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"

if ! command -v bun &> /dev/null; then
    if [ "$1" = "session-start" ]; then
        echo '{"systemMessage": "To set up hive-mind: run /hive-mind:setup"}'
        exit 0
    else
        echo "To set up hive-mind: run /hive-mind:setup"
        exit 1
    fi
fi

exec bun "$PLUGIN_ROOT/cli.js" "$@"
