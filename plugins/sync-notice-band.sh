#!/bin/bash
set -euo pipefail

# Copies one plugin's hooks/notice-band.tsx over every other plugin's copy, which
# notice-band.test.ts keeps identical. Usage: sync-notice-band.sh <plugin you edited>

cd "$(dirname "${BASH_SOURCE[0]}")"
SOURCE="${1:?usage: sync-notice-band.sh <plugin you edited>}/hooks/notice-band.tsx"
[ -f "$SOURCE" ] || { echo "no $SOURCE" >&2; exit 1; }

for copy in */hooks/notice-band.tsx; do
  [ "$copy" = "$SOURCE" ] || cp "$SOURCE" "$copy"
done
