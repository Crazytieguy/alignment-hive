#!/bin/sh
# Schedules run.sh hourly with launchd (macOS). `sh install.sh` installs or updates the job; `sh install.sh --remove`
# removes it. The job runs from this checkout, so install from the main checkout, not a worktree. It keeps this
# shell's PATH (where claude, bun, docker, uv and gh are found) and any XDG_* or CANARY_DATA overrides.
set -eu
label="com.alignment-hive.model-router-canary"
plist="$HOME/Library/LaunchAgents/$label.plist"
here="$(cd "$(dirname "$0")" && pwd)"
data="${CANARY_DATA:-$HOME/.local/share/alignment-hive-model-router-canary}"
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
for name in XDG_CONFIG_HOME XDG_STATE_HOME XDG_CACHE_HOME CANARY_DATA; do
  value="$(printenv "$name" || true)"
  if [ -n "$value" ]; then
    env_xml="$env_xml
    <key>$name</key><string>$(xml "$value")</string>"
  fi
done
cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array><string>/bin/sh</string><string>$here/run.sh</string></array>
  <key>StartInterval</key><integer>3600</integer>
  <key>EnvironmentVariables</key><dict>
$env_xml
  </dict>
  <key>StandardOutPath</key><string>$data/launchd.log</string>
  <key>StandardErrorPath</key><string>$data/launchd.log</string>
</dict>
</plist>
EOF
launchctl bootstrap "$domain" "$plist"
echo "Installed $label: hourly. Run now: launchctl kickstart $domain/$label"
