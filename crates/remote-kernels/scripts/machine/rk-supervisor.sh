#!/usr/bin/env bash
# The machine's own cleanup, for when no server is there to do it.
#
# Files in the state dir, all plain and all overwritten by a server's install:
#   alive        "<owner> <epoch>": the server driving this machine, and when it
#                last said so. The clock is this machine's.
#   deadline     absolute epoch when the budget runs out; absent without a budget
#   intent.json  the queued finish() plan; absent without one
#   stage.sh     copies queued downloads somewhere a stop will not wipe
#
# No file here ever means "refuse". Every action is an idempotent provider call,
# and whoever installs next replaces this process (supervisor.pid: last one wins).
#
# Usage: rk-supervisor.sh <dir> install <owner> <refuse_foreign_within|-> <deadline_secs|-> <settings>
#        rk-supervisor.sh <dir> supervise <settings>
# Settings: <stale_secs> <grace_secs> <wait_secs> <save_timeout_secs> <port> <token>
#           <stop|terminate> <save_cmd|-> <action_cmd|->
# An action_cmd of "-" installs no supervisor, only the alive file.

set -u
umask 077

dir="$1"
mode="$2"
shift 2

log() {
    printf "%s %s\n" "$(date +%s)" "$*"
}

write_atomic() {
    local tmp="$dir/.$(basename "$1").$$.tmp"
    printf "%s\n" "$2" > "$tmp" && mv -f "$tmp" "$1"
}

is_uint() {
    case "${1:-}" in
        ""|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

# Sleep between polls, unless this process has been replaced.
pause() {
    [ "$(cat "$dir/supervisor.pid" 2>/dev/null)" = "$$" ] || exit 0
    if [ -n "${RK_SUPERVISOR_PARENT:-}" ]; then
        kill -0 "$RK_SUPERVISOR_PARENT" 2>/dev/null || exit 0
    fi
    sleep "${RK_SUPERVISOR_POLL_SECS:-2}"
}

# Keeps the last good timestamp if the file cannot be read: a damaged file must
# not read as a dead server any sooner than a silent one would.
is_fresh() {
    local owner ts rest
    { read -r owner ts rest < "$dir/alive"; } 2>/dev/null
    is_uint "${ts:-}" && alive_ts="$ts"
    [ $(($(date +%s) - alive_ts)) -lt "$stale_secs" ]
}

budget_due() {
    local deadline=""
    { read -r deadline < "$dir/deadline"; } 2>/dev/null
    is_uint "$deadline" && [ "$(date +%s)" -ge "$deadline" ]
}

# A reachable server moving the deadline is the only thing that cancels the
# budget path; a fresh alive is what cancels the stale one.
cancelled() {
    if [ "$1" = "budget" ]; then ! budget_due; else is_fresh; fi
}

jupyter_is_idle() {
    local body compact
    body=$(curl -sS --max-time 5 -H "Authorization: token $token" \
        "http://127.0.0.1:$port/api/kernels" 2>/dev/null) || return 1
    compact=$(printf "%s" "$body" | tr -d " \n\r\t")
    case "$compact" in
        "[]") return 0 ;;
        \[*\"execution_state\":\"*\]) ;;
        *) return 1 ;;
    esac
    case "$(printf "%s" "$compact" | sed "s/\"execution_state\":\"idle\"//g")" in
        *\"execution_state\":\"*) return 1 ;;
    esac
}

# Run a command for at most $1 seconds; 124 means it was cut off.
run_bounded() {
    local limit="$1" pid end
    shift
    "$@" &
    pid=$!
    end=$(($(date +%s) + limit))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$(date +%s)" -ge "$end" ]; then
            kill -TERM "$pid" 2>/dev/null
            sleep 1
            kill -KILL "$pid" 2>/dev/null
            wait "$pid" 2>/dev/null
            return 124
        fi
        sleep 1
    done
    wait "$pid"
}

# Sets save_ok. A configured save that was skipped, failed or timed out leaves
# save_ok=0, and select_action will not delete the machine after that.
run_save() {
    local limit="$1" status=0
    save_ok=1
    [ "$save_cmd" != "-" ] || return 0
    if [ "$limit" -le 0 ]; then
        log "save skipped: no time left"
        save_ok=0
        return 0
    fi
    log "save starting (limit ${limit}s)"
    run_bounded "$limit" bash -lc "$save_cmd" || status=$?
    log "save finished: exit $status"
    [ "$status" -eq 0 ] || save_ok=0
}

# The one rule both paths use. $1 is "budget" or "stale". Sets action.
select_action() {
    local compact requested
    action="$default_action"
    if [ -f "$dir/intent.json" ]; then
        compact=$(tr -d " \n\r\t" < "$dir/intent.json" 2>/dev/null)
        requested=$(printf "%s\n" "$compact" | sed -n "s/.*\"then\":\"\([a-z]*\)\".*/\1/p")
        case "$requested" in
            stop|terminate|keep) action="$requested" ;;
            *) action="stop" ;;
        esac
        case "$compact" in
            *\"downloads_pending\":true*) [ "$action" != "terminate" ] || action="stop" ;;
        esac
    fi
    if [ "$1" = "budget" ] && [ "$action" = "keep" ]; then
        action="stop"
    fi
    if [ "$save_ok" -eq 0 ] && [ "$action" = "terminate" ]; then
        action="stop"
    fi
}

stage_downloads() {
    [ "$action" = "stop" ] && [ -f "$dir/stage.sh" ] || return 0
    run_bounded "$save_timeout_secs" bash "$dir/stage.sh" >/dev/null 2>&1 || log "staging downloads failed"
}

# Run the action, then again every minute for as long as this process lives: a
# provider call that failed must not leave a billing machine with no supervisor.
act() {
    while true; do
        if cancelled "$1"; then
            log "$1: $action cancelled"
            return 0
        fi
        log "$1: running $action"
        if bash -lc "$action_cmd" rk-action "$action"; then
            log "$action call succeeded"
        else
            log "$action call failed"
        fi
        sleep "${RK_SUPERVISOR_RETRY_SECS:-60}"
        pause
    done
}

# $2 seconds from now the drain stops waiting; 0 means it never does.
drain() {
    local path="$1" limit="$2" started
    started=$(date +%s)
    until jupyter_is_idle; do
        if [ "$limit" -gt 0 ] && [ $(($(date +%s) - started)) -ge "$limit" ]; then
            log "drain gave up after ${limit}s"
            return 0
        fi
        pause
        cancelled "$path" && return 1
        if [ "$path" = "stale" ] && budget_due; then
            return 1
        fi
    done
}

# Drain, save, decide, act. The budget path gives a reachable server its whole
# grace to move the deadline; the stale path stands down the moment one is back.
cleanup() {
    local path="$1" grace_end save_limit="$save_timeout_secs"
    save_ok=1
    if [ "$path" = "budget" ]; then
        log "budget deadline passed"
        grace_end=$(($(date +%s) + grace_secs))
        drain budget "$grace_secs" || return 0
        [ $((grace_end - $(date +%s))) -ge "$save_limit" ] || save_limit=$((grace_end - $(date +%s)))
    else
        log "no server for ${stale_secs}s"
        drain stale "$wait_secs" || return 0
        select_action stale
        if [ "$action" = "keep" ]; then
            log "finish plan says keep; waiting for a server"
            until is_fresh || budget_due; do
                pause
            done
            return 0
        fi
    fi
    run_save "$save_limit"
    select_action "$path"
    if [ "$path" = "budget" ]; then
        while [ "$(date +%s)" -lt "$grace_end" ]; do
            cancelled budget && return 0
            pause
        done
    fi
    cancelled "$path" && return 0
    stage_downloads
    act "$path"
}

supervise() {
    printf "%s\n" "$$" > "$dir/supervisor.pid"
    alive_ts=$(date +%s)
    log "supervising (stale ${stale_secs}s, default $default_action)"
    while true; do
        if budget_due; then
            cleanup budget
        elif ! is_fresh; then
            cleanup stale
        fi
        pause
    done
}

# Kill what a pid file names, only if it still is that program: a pid from
# before a stop can belong to anything now.
kill_if_ours() {
    local pid command
    pid=$(cat "$1" 2>/dev/null)
    is_uint "$pid" && [ "$pid" != "$$" ] || return 0
    command=$(tr "\0" " " < "/proc/$pid/cmdline" 2>/dev/null || ps -p "$pid" -o command= 2>/dev/null)
    case "$command" in
        *"$2"*) kill -TERM "$pid" 2>/dev/null ;;
    esac
    return 0
}

install() {
    local owner="$1" refuse_within="$2" deadline_secs="$3" now current_owner ts rest
    shift 3
    local action_arg="${9:-}"
    if [ "$action_arg" != "-" ]; then
        for tool in curl sed nohup; do
            if ! command -v "$tool" >/dev/null 2>&1; then
                echo "unsupervisable the machine lacks $tool"
                return 0
            fi
        done
    fi
    now=$(date +%s)
    if [ "$refuse_within" != "-" ]; then
        current_owner=""
        ts=""
        { read -r current_owner ts rest < "$dir/alive"; } 2>/dev/null
        if is_uint "$ts" && [ -n "$current_owner" ] && [ "$current_owner" != "$owner" ] \
            && [ $((now - ts)) -lt "$refuse_within" ]; then
            echo "foreign $current_owner $((now - ts))"
            return 0
        fi
    fi
    write_atomic "$dir/alive" "$owner $now" || exit 1
    if [ "$deadline_secs" = "-" ]; then
        rm -f "$dir/deadline"
    else
        write_atomic "$dir/deadline" "$((now + deadline_secs))" || exit 1
    fi
    kill_if_ours "$dir/supervisor.pid" rk-supervisor
    # What an older version of this tool left behind.
    kill_if_ours "$dir/watchdog.pid" rk-watchdog
    rm -f "$dir/lease.json" "$dir/lease.lock" "$dir/watchdog.pid" "$dir/watchdog.lock" \
        "$dir/heartbeat" "$dir/budget_deadline" "$dir/outcome.json" "$dir"/outcome.*.json \
        "$dir/rk-lease.sh" "$dir/rk-watchdog.sh"
    if [ "$action_arg" = "-" ]; then
        rm -f "$dir/supervisor.pid"
    else
        # The pid file is the supervisor's to write, but "install" must not
        # report success with the old pid still in it.
        nohup bash "$0" "$dir" supervise "$@" </dev/null >>"$dir/supervisor.log" 2>&1 &
        printf "%s\n" "$!" > "$dir/supervisor.pid"
    fi
    echo "installed"
}

if [ "$mode" = "install" ]; then
    install "$@"
    exit 0
fi

stale_secs="$1"
grace_secs="$2"
wait_secs="$3"
save_timeout_secs="$4"
port="$5"
token="$6"
default_action="$7"
save_cmd="$8"
action_cmd="$9"
supervise
