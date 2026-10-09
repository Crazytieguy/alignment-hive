#!/bin/bash
set -euo pipefail

# Run once per session by hooks/register.tsx: prints the band's rows as JSON
# ({"notices": [...]}, the Notice shape in hooks/notice-band.tsx), or nothing.
# mats has no dependency on hive; this nudge exists to onboard fellows to the
# alignment-hive install script. The mod leaves it to the hive plugin when
# that is enabled, since hive draws the same row.
if ! command -v hive >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/hive" ]; then
  echo '{"notices": [{"id": "install", "severity": "action", "text": "alignment-hive CLI not installed", "actions": [{"id": "install", "label": "Copy install command", "kind": "copy", "command": "curl -fsSL https://alignment-hive.com/install.sh | bash"}]}]}'
fi

exit 0
