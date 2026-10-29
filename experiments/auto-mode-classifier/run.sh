#!/bin/sh
# Runs bench.ts without the calling session's proxy wiring (a Claude Code session exports HTTPS_PROXY at the live
# router; the bench must never touch it). Usage: sh run.sh <bench.ts arguments>
here="$(cd "$(dirname "$0")" && pwd)"
exec env -u HTTPS_PROXY -u HTTP_PROXY -u ALL_PROXY -u NO_PROXY -u https_proxy -u http_proxy -u all_proxy -u no_proxy bun "$here/bench.ts" "$@"
