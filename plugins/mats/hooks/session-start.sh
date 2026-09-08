#!/bin/bash
set -euo pipefail

# mats has no dependency on hive; this nudge exists to onboard fellows to the
# alignment-hive install script.
if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  echo '{"systemMessage": "\u001b[1;36mmats:\u001b[0m set up alignment-hive, run \u001b[1;35m$ curl -fsSL https://alignment-hive.com/install.sh | bash\u001b[0m"}'
fi

exit 0
