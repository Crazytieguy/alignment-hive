#!/bin/bash
# Run once per session by hooks/register.tsx: prints the band's rows as JSON
# ({"notices": [...]}, the Notice shape in hooks/notice-band.tsx), or nothing.
# Quiet when everything is healthy (including silent service refreshes after
# plugin updates and silent restarts of a stopped service). Speaks up only
# when the user needs to act. Never fails the session.

BOOTSTRAP="${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh"
VERSION_FILE="${CLAUDE_PLUGIN_ROOT}/binary-version"
PLUGIN_VERSION=$(tr -d '[:space:]' < "$VERSION_FILE" 2>/dev/null || echo "")

# One row whose button runs /model-router:setup: id, severity, text, button label.
setup_notice() {
  printf '{"notices": [{"id": "%s", "severity": "%s", "text": "%s", "actions": [{"id": "setup", "label": "%s", "kind": "command", "command": "/model-router:setup"}]}]}\n' "$1" "$2" "$3" "$4"
}

# Wrong platform-specific variant installed: nothing below can work, so say it
# before anything else. bootstrap.sh owns the test.
if ! bash "$BOOTSTRAP" platform-check >/dev/null 2>&1; then
  setup_notice platform problem "wrong platform variant installed" "Fix"
  exit 0
fi

BASE="${ANTHROPIC_BASE_URL:-}"

if [ -z "$BASE" ]; then
  setup_notice setup action "not configured" "Run setup"
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
    (nohup bash "$BOOTSTRAP" service refresh >/dev/null 2>&1 &) 2>/dev/null
  fi
  # The /model picker lacks a shipped GPT route or lists a retired one
  # (`settings models` decides; skipped while the service is on another
  # version, so session start never waits on a binary download). Add to /model
  # runs `settings models --apply` (hooks/register.tsx); until then, a row each
  # session, unless dismissed for this plugin version.
  if [ "$RUNNING_VERSION" = "$PLUGIN_VERSION" ] &&
    MODELS=$(bash "$BOOTSTRAP" settings models --project-dir "${CLAUDE_PROJECT_DIR:-$PWD}" 2>/dev/null) &&
    printf '%s' "$MODELS" | grep -q '"needed":true'; then
    # "GPT-6.1 Sol" or "GPT-6 Astra, GPT-6.1 Sol and GPT-6 Luna".
    LABELS=$(printf '%s' "$MODELS" | sed -n 's/.*"labels":\[\([^]]*\)\].*/\1/p' | sed 's/","/, /g; s/"//g; s/\(.*\), /\1 and /')
    if [ -n "$LABELS" ]; then
      TEXT="$LABELS can be added to /model"
      LABEL="Add to /model"
    else
      TEXT="/model still lists retired GPT models"
      LABEL="Update /model"
    fi
    if printf '%s' "$MODELS" | grep -q '"canApply":true'; then
      ACTIONS='{"id": "add-models", "label": "'"$LABEL"'", "kind": "plugin"}, {"id": "customize", "label": "Customize", "kind": "command", "command": "/model-router:setup"}'
    else
      ACTIONS='{"id": "setup", "label": "Run setup", "kind": "command", "command": "/model-router:setup"}'
    fi
    printf '{"notices": [{"id": "new-model", "severity": "action", "text": "%s", "version": "%s", "actions": [%s]}]}\n' \
      "$TEXT" "$PLUGIN_VERSION" "$ACTIONS"
  fi
  exit 0
fi

# Configured but unreachable: one silent restart attempt, then warn.
bash "$BOOTSTRAP" service restart >/dev/null 2>&1
sleep 1
if curl -sf --max-time 2 "${BASE%/}/__model-router/health" >/dev/null 2>&1; then
  exit 0
fi
# No setup button: setup is a skill, and the model is out of reach while the router is down.
# Bypass router takes ANTHROPIC_BASE_URL out of the settings (hooks/register.tsx); Try again runs
# this check once more, which is one more restart attempt.
printf '{"notices": [{"id": "down", "severity": "urgent", "text": "%s", "actions": [{"id": "bypass", "label": "Bypass router", "kind": "plugin"}, {"id": "retry", "label": "Try again", "kind": "plugin"}]}]}\n' \
  "not running and could not be restarted, so requests fail"
exit 0
