#!/bin/bash
set -euo pipefail

# Check if hive binary is installed (installed via alignment-hive install script)
if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  echo '{"systemMessage": "\u001b[1;36mmats:\u001b[0m set up alignment-hive, run \u001b[1;35m$ curl -fsSL https://alignment-hive.com/install.sh | bash\u001b[0m"}'
fi

exit 0
