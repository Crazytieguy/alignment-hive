#!/usr/bin/env bash
set -u

if [ -f "$RK_STATE_DIR/outcome.json" ]; then
    marker="present"
else
    marker="missing"
fi
printf "outcome=%s action=%s\n" "$marker" "$1" >> "$RK_ACTION_LOG"
