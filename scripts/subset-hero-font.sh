#!/bin/zsh
# Regenerates packages/web/src/assets/fraunces-hero.woff2, the landing hero's
# display font: fontsource's latin Fraunces variable font with the weight axis
# pinned to the H1's 520 (the optical-size axis stays variable so optical
# sizing follows the H1's responsive size) and glyphs cut down to Latin-1 plus
# common punctuation. Characters outside that range fall back to Georgia.
#
# Needs uv (uvx fetches fonttools). Run from the repo root after a
# @fontsource-variable/fraunces update.
set -euo pipefail
cd "$(dirname "$0")/.."

SRC=node_modules/@fontsource-variable/fraunces/files/fraunces-latin-opsz-normal.woff2
OUT=packages/web/src/assets/fraunces-hero.woff2
RANGES="U+0020-007E,U+00A0-00FF,U+2010-2027,U+2032-2033"
TMP=$(mktemp -t fraunces-hero.XXXXXX.woff2)
trap 'rm -f "$TMP"' EXIT

uvx --from 'fonttools[woff]' fonttools varLib.instancer -q -o "$TMP" "$SRC" wght=520
uvx --from 'fonttools[woff]' pyftsubset "$TMP" \
  --unicodes="$RANGES" --flavor=woff2 --layout-features='*' \
  --output-file="$OUT"
ls -l "$OUT"
