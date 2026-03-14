#!/bin/bash
set -euo pipefail

# Check if hive binary is installed (installed via alignment-hive install script)
if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  echo '{"systemMessage": "mats: Set up alignment-hive: curl -fsSL https://alignment-hive.com/install.sh | bash"}'
fi

exit 0
