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
# Claude Code takes the lowercase variable first.
PROXY="${https_proxy:-${HTTPS_PROXY:-}}"
PROJECT="${CLAUDE_PROJECT_DIR:-$PWD}"
TOKEN=$(tr -d '[:space:]' < "${XDG_STATE_HOME:-$HOME/.local/state}/model-router/ingress-token" 2>/dev/null || echo "")

# Which wiring this session runs on, and the router URL it implies. Only a
# gateway that is actually ours is managed: a base URL must carry this
# install's ingress token, and the proxy wiring needs the token for its
# health URL. Another gateway, a loopback proxy of someone else's (or a token
# pinned in the config that we can't read here) is left alone: no restarts,
# no advice.
case "$BASE" in
  http://127.*|http://\[::1\]*)
    case "$BASE" in
      *"/t/$TOKEN"*) [ -n "$TOKEN" ] || exit 0 ;;
      *) exit 0 ;;
    esac
    WIRING=base-url
    ROUTER="${BASE%/}"
    ;;
  ""|https://api.anthropic.com|https://api.anthropic.com/)
    # The desktop app pins the second.
    case "$PROXY" in
      http://127.*|http://localhost*|http://\[::1\]*) ;;
      *) PROXY="" ;;
    esac
    if [ -n "$PROXY" ] && [ -n "${CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER:-}" ] && [ -n "$TOKEN" ]; then
      WIRING=proxy
      ROUTER="${PROXY%/}/t/$TOKEN"
    elif [ -n "$TOKEN" ] && grep -qsF "/t/$TOKEN" "$HOME/.claude/settings.json" \
      "$PROJECT/.claude/settings.json" "$PROJECT/.claude/settings.local.json"; then
      # The settings carry this install's base URL, yet the session runs
      # without it: the desktop app drops it in favour of its own.
      setup_notice desktop action "is off in the desktop app, which ignores ANTHROPIC_BASE_URL; setup can wire it as a proxy instead" "Run setup"
      exit 0
    else
      setup_notice setup action "not configured" "Run setup"
      exit 0
    fi
    ;;
  # Some other gateway; stay out of the way.
  *) exit 0 ;;
esac

# Straight to the router, past any proxy (it may be the router itself).
HEALTH=$(curl -sf --noproxy '*' --max-time 2 "$ROUTER/__model-router/health" 2>/dev/null || echo "")

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
  # Settings migrations: `settings <operation>` decides each, skipped while the
  # service is on another version (so session start never waits on a binary
  # download). Its button runs `settings <operation> --apply`
  # (hooks/register.tsx); until then, a row each session, unless dismissed for
  # this plugin version.
  ROWS=""
  # Whether `settings $1` needs a change, its report left in OUT.
  needed() {
    [ "$RUNNING_VERSION" = "$PLUGIN_VERSION" ] &&
      OUT=$(bash "$BOOTSTRAP" settings "$1" --project-dir "$PROJECT" 2>/dev/null) &&
      printf '%s' "$OUT" | grep -q '"needed":true'
  }
  # Adds the row for OUT: id, text, and the button's action id and label,
  # offered with Customize when the binary can apply it, else Run setup. A
  # dismissal holds until the plugin's next version, or until $5 changes.
  add_row() {
    if printf '%s' "$OUT" | grep -q '"canApply":true'; then
      ACTIONS='{"id": "'"$3"'", "label": "'"$4"'", "kind": "plugin"}, {"id": "customize", "label": "Customize", "kind": "command", "command": "/model-router:setup"}'
    else
      ACTIONS='{"id": "setup", "label": "Run setup", "kind": "command", "command": "/model-router:setup"}'
    fi
    ROWS=$(printf '%s{"id": "%s", "severity": "action", "text": "%s", "version": "%s", "actions": [%s]},' \
      "$ROWS" "$1" "$2" "${5:-$PLUGIN_VERSION}" "$ACTIONS")
  }
  # The /model picker lacks a shipped GPT route or lists a retired one.
  # Adding a route sets its window too, so while this row shows (it is not
  # dismissed, hooks/register.tsx says in NEW_MODEL_DISMISSED), the windows
  # row below leaves its routes out: ADDS, comma-separated.
  ADDS=""
  if needed models; then
    [ "${NEW_MODEL_DISMISSED:-}" = "$PLUGIN_VERSION" ] ||
      ADDS=$(printf '%s' "$OUT" | sed -n 's/.*"adds":\[\([^]]*\)\].*/\1/p' | tr -d '"')
    # "GPT-6.1 Terra" or "GPT-6 Astra, GPT-6.1 Sol, GPT-6.1 Terra and GPT-6 Luna".
    LABELS=$(printf '%s' "$OUT" | sed -n 's/.*"labels":\[\([^]]*\)\].*/\1/p' | sed 's/","/, /g; s/"//g; s/\(.*\), /\1 and /')
    if [ -n "$LABELS" ]; then
      add_row new-model "$LABELS can be added to /model" add-models "Add to /model"
    else
      add_row new-model "/model still lists retired GPT models" add-models "Update /model"
    fi
  fi
  # The next session's context windows are not setup's: the old setup's single
  # declaration, usage scaling or a behavesAs row (`legacy`), a route with no
  # compaction window, or one still at a Codex default that has since moved
  # (`updates`).
  if needed windows; then
    ALL_ROUTES=$(printf '%s' "$OUT" | sed -n 's/.*"routes":\[\([^]]*\)\].*/\1/p' | tr -d '"')
    ROUTES=""
    for ROUTE in $(printf '%s' "$ALL_ROUTES" | tr ',' ' '); do
      case ",$ADDS," in
        *",$ROUTE,"*) ;;
        *) ROUTES="${ROUTES:+$ROUTES, }$ROUTE" ;;
      esac
    done
    UPDATES=$(printf '%s' "$OUT" | sed -n 's/.*"updates":\[\([^]]*\)\].*/\1/p' | sed 's/","/, /g; s/"//g')
    if printf '%s' "$OUT" | grep -q '"legacy":true'; then
      add_row windows "context windows are still set the old way" windows "Migrate"
    elif [ -n "$ROUTES" ] && [ -n "$UPDATES" ]; then
      add_row windows "no context window set for $ROUTES; Codex raised the default context window of $UPDATES" windows "Set"
    elif [ -n "$ROUTES" ]; then
      add_row windows "no context window set for $ROUTES" windows "Set"
    elif [ -n "$UPDATES" ]; then
      # Its own row, dismissed until the routes change rather than until the
      # next version: someone keeping the old window chose it.
      add_row windows-update "Codex raised the default context window of $UPDATES" windows "Update" "updates:$UPDATES"
    elif [ -z "$ALL_ROUTES" ]; then
      add_row windows "context windows need setup" windows "Set"
    fi
  fi
  SETUP='{"id": "setup", "label": "Run setup", "kind": "command", "command": "/model-router:setup"}'
  if [ "$WIRING" = base-url ]; then
    # The base-URL wiring keeps Remote Control off and the desktop app away
    # from the router. Setup moves it to the HTTPS proxy: not a button's
    # edit, as macOS asks for a password to trust the router's CA. Offered
    # only where setup can finish it; a conflict (another proxy, a NO_PROXY
    # exempting Anthropic) stays quiet, and doctor names it.
    if needed proxy && printf '%s' "$OUT" | grep -qE '"canApply":true|"untrusted":true'; then
      ROWS=$(printf '%s{"id": "proxy", "severity": "action", "text": "%s", "version": "%s", "actions": [%s]},' \
        "$ROWS" "can keep Remote Control on and route in the desktop app too" "$PLUGIN_VERSION" "$SETUP")
    fi
  elif [ "$RUNNING_VERSION" = "$PLUGIN_VERSION" ] &&
    OUT=$(bash "$BOOTSTRAP" settings proxy --project-dir "$PROJECT" 2>/dev/null) &&
    printf '%s' "$OUT" | grep -q '"reason"'; then
    # The proxy wiring, but the next session would fail it (an untrusted
    # CA fails every request) or skip it (another proxy, a NO_PROXY).
    ROWS=$(printf '%s{"id": "proxy-broken", "severity": "problem", "text": "%s", "actions": [%s, %s]},' \
      "$ROWS" "isn't routing: the HTTPS-proxy wiring is not in effect (model-router doctor says why)" "$SETUP" \
      '{"id": "bypass", "label": "Bypass router", "kind": "plugin"}')
  fi
  [ -n "$ROWS" ] && printf '{"notices": [%s]}\n' "${ROWS%,}"
  exit 0
fi

# Configured but unreachable: one silent restart attempt, then warn.
bash "$BOOTSTRAP" service restart >/dev/null 2>&1
sleep 1
if curl -sf --noproxy '*' --max-time 2 "$ROUTER/__model-router/health" >/dev/null 2>&1; then
  exit 0
fi
# No setup button: setup is a skill, and the model is out of reach while the router is down
# (under the proxy wiring, Anthropic too). Bypass router takes the wiring out of the settings
# (hooks/register.tsx); Try again runs this check once more, which is one more restart attempt.
printf '{"notices": [{"id": "down", "severity": "urgent", "text": "%s", "actions": [{"id": "bypass", "label": "Bypass router", "kind": "plugin"}, {"id": "retry", "label": "Try again", "kind": "plugin"}]}]}\n' \
  "not running and could not be restarted, so requests fail"
exit 0
