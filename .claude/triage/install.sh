#!/bin/sh
# Schedules run.sh daily at 04:00 local time with launchd (macOS). `sh install.sh` installs or updates the job;
# `sh install.sh --remove` removes it. The job runs from this checkout, so install from the main checkout, not a
# worktree. It keeps this shell's PATH (where claude, bun and gh are found) and any TRIAGE_DATA override.
set -eu
label="com.alignment-hive.triage"
plist="$HOME/Library/LaunchAgents/$label.plist"
here="$(cd "$(dirname "$0")" && pwd)"
data="${TRIAGE_DATA:-$HOME/.local/share/alignment-hive-triage}"
domain="gui/$(id -u)"

if [ "${1:-}" = "--remove" ]; then
  launchctl bootout "$domain/$label" 2>/dev/null || true
  rm -f "$plist"
  echo "Removed $label."
  exit 0
fi

case "$here" in
  */.claude/worktrees/*) echo "Run this from the main checkout: launchd would keep running a worktree that gets deleted." >&2; exit 1 ;;
esac
launchctl bootout "$domain/$label" 2>/dev/null || true

mkdir -p "$data" "$HOME/Library/LaunchAgents"
xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }
env_xml="    <key>PATH</key><string>$(xml "$PATH")</string>"
if [ -n "${TRIAGE_DATA:-}" ]; then
  env_xml="$env_xml
    <key>TRIAGE_DATA</key><string>$(xml "$TRIAGE_DATA")</string>"
fi
cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <!-- launchd catches up missed calendar events on wake; skip those outside the 04:00 hour. -->
  <key>ProgramArguments</key><array>
    <string>/bin/sh</string><string>-c</string>
    <string>[ "\$(date +%H)" = 04 ] || exit 0; exec /bin/sh "\$1"</string>
    <string>$label</string><string>$here/run.sh</string>
  </array>
  <key>StartCalendarInterval</key><dict>
    <key>Hour</key><integer>4</integer>
    <key>Minute</key><integer>0</integer>
  </dict>
  <key>EnvironmentVariables</key><dict>
$env_xml
  </dict>
  <key>StandardOutPath</key><string>$data/launchd.log</string>
  <key>StandardErrorPath</key><string>$data/launchd.log</string>
</dict>
</plist>
EOF
launchctl bootstrap "$domain" "$plist"
echo "Installed $label: daily at 04:00 local time (late wakes outside the 04:00 hour are skipped). Run now: sh $here/run.sh"
