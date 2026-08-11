#!/bin/bash
# SessionStart hook for model-router.
# Quiet when everything is healthy (including silent service refreshes after
# plugin updates and silent restarts of a stopped service). Speaks up only
# when the user needs to act. Never fails the session.
#
# Messages use ONLY textual backslash-u001b escapes (JSON-decoded by Claude Code);
# this file must contain no literal control bytes.

BOOTSTRAP="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"
VERSION_FILE="${CLAUDE_PLUGIN_ROOT}/binary-version"
PLUGIN_VERSION=$({ [ -f "$VERSION_FILE" ] && tr -d '[:space:]' < "$VERSION_FILE"; } 2>/dev/null || echo "")

U='\'"u001b"
BOLD="${U}[1m"
MAGENTA="${U}[1;35m"
RESET="${U}[0m"

msg() {
  # The argument may contain the textual escape variables above; printf %s
  # passes them through verbatim into the JSON string.
  printf '{"systemMessage": "%smodel-router:%s %s"}\n' "$BOLD" "$RESET" "$1"
}

# Wrong platform-specific variant installed: nothing below can work, so say it
# before anything else. bootstrap.sh owns the test.
if ! bash "$BOOTSTRAP" platform-check >/dev/null 2>&1; then
  msg "wrong platform, run ${MAGENTA}/model-router:setup${RESET} to fix"
  exit 0
fi

BASE="${ANTHROPIC_BASE_URL:-}"

if [ -z "$BASE" ]; then
  msg "not configured, run ${MAGENTA}/model-router:setup${RESET}"
  exit 0
fi

# A non-loopback base URL is some other gateway; stay out of the way.
case "$BASE" in
  http://127.*|http://\[::1\]*) ;;
  *) exit 0 ;;
esac

# Only manage a gateway that is actually ours: the base URL must carry this
# install's ingress token. Another loopback proxy (or a token pinned in the
# config that we can't read here) is left alone — no restarts, no advice.
TOKEN=$(tr -d '[:space:]' < "${XDG_STATE_HOME:-$HOME/.local/state}/model-router/ingress-token" 2>/dev/null || echo "")
case "$BASE" in
  *"/t/$TOKEN"*) [ -n "$TOKEN" ] || exit 0 ;;
  *) exit 0 ;;
esac

HEALTH=$(curl -sf --max-time 2 "${BASE%/}/__model-router/health" 2>/dev/null || echo "")

if [ -n "$HEALTH" ]; then
  RUNNING_VERSION=$(printf '%s' "$HEALTH" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  if [ -n "$PLUGIN_VERSION" ] && [ -n "$RUNNING_VERSION" ] && [ "$RUNNING_VERSION" != "$PLUGIN_VERSION" ]; then
    # Plugin updated: refresh the launcher and restart onto the new binary,
    # in the background so session start never blocks on the binary
    # download (the download itself is atomic, so an interrupt is safe).
    # Silent either way — on failure (release may still be building) the
    # refresh aborts before touching the launcher, the current service keeps
    # running, and the next session retries; a stuck mismatch is surfaced by
    # `model-router doctor`.
    (nohup bash "$BOOTSTRAP" service refresh --plugin-root "$CLAUDE_PLUGIN_ROOT" >/dev/null 2>&1 &) 2>/dev/null
  fi
  exit 0
fi

# Configured but unreachable: one silent restart attempt, then warn.
bash "$BOOTSTRAP" service restart >/dev/null 2>&1
sleep 1
if curl -sf --max-time 2 "${BASE%/}/__model-router/health" >/dev/null 2>&1; then
  exit 0
fi
msg "not running and restart failed - remove ANTHROPIC_BASE_URL from your Claude settings to get back online, then run ${MAGENTA}/model-router:setup${RESET}"
exit 0
