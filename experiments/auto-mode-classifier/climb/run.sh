#!/bin/sh
# Runs climb.ts (or, with `test`, the climb unit tests) without the calling session's proxy wiring, as ../run.sh does
# for bench.ts. Usage: sh climb/run.sh <climb.ts arguments> | sh climb/run.sh test [bun test arguments]
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/.." || exit 1
if [ "$1" = test ]; then
  shift
  exec env -u HTTPS_PROXY -u HTTP_PROXY -u ALL_PROXY -u NO_PROXY -u https_proxy -u http_proxy -u all_proxy -u no_proxy bun test climb "$@"
fi
exec env -u HTTPS_PROXY -u HTTP_PROXY -u ALL_PROXY -u NO_PROXY -u https_proxy -u http_proxy -u all_proxy -u no_proxy bun "$here/climb.ts" "$@"
