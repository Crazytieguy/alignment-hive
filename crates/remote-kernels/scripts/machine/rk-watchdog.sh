#!/usr/bin/env bash
# Detached supervisor for the active -> armed -> finalizing lease machine. It
# generation-gates heartbeats, drains Jupyter, records outcome.json, enters the
# fenced finalizing state, then executes the provider action. Exit codes: 0
# success/already installed, 11 bad config, 12 missing flock, 13 unusable state
# directory, 14 missing dependency; lease exits 9/10 abort an action cleanly.
# This file contains no single quotes for later single-quote-wrapped SSH transport.
# heartbeat is "<generation> <epoch>"; budget_deadline is one absolute epoch.
# install is the supported entry; supervise is internal. An optional final
# install argument supplies the hourly storage rate written for stop outcomes.

set -u
umask 077

readonly EXIT_FENCED=9
readonly EXIT_REFUSED=10
readonly EXIT_INVALID=11
readonly EXIT_NO_FLOCK=12
readonly EXIT_BAD_STATE_DIR=13
readonly EXIT_MISSING_DEP=14

usage() {
    printf "%s\n" "usage: rk-watchdog.sh <state_dir> check-prereqs" >&2
    printf "%s\n" "   or: rk-watchdog.sh <state_dir> install <lease_script> <stale_secs> <grace_secs> <finalize_wait_secs> <finalize_timeout_secs> <port> <token> <stop|terminate> <finalize_cmd|-> <action_cmd> [storage_rate_per_hour]" >&2
    exit "$EXIT_INVALID"
}

is_uint() {
    case "${1:-}" in
        ""|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

is_rate() {
    local whole fraction
    is_uint "${1:-}" && return 0
    case "${1:-}" in
        *.*)
            whole=${1%%.*}
            fraction=${1#*.}
            is_uint "$whole" && is_uint "$fraction"
            ;;
        *) return 1 ;;
    esac
}

check_prereqs() {
    command -v flock >/dev/null 2>&1 || exit "$EXIT_NO_FLOCK"
    command -v curl >/dev/null 2>&1 || exit "$EXIT_MISSING_DEP"
    command -v sed >/dev/null 2>&1 || exit "$EXIT_MISSING_DEP"
    command -v nohup >/dev/null 2>&1 || exit "$EXIT_MISSING_DEP"
    command -v sync >/dev/null 2>&1 || exit "$EXIT_MISSING_DEP"
    mkdir -p "$state_dir" >/dev/null 2>&1 || exit "$EXIT_BAD_STATE_DIR"
    probe="$state_dir/.watchdog-write-check.$$"
    : > "$probe" 2>/dev/null || exit "$EXIT_BAD_STATE_DIR"
    rm -f "$probe" || exit "$EXIT_BAD_STATE_DIR"
}

json_string_text() {
    local text="$1"
    local key="$2"
    printf "%s\n" "$text" | sed -n "s/.*\"${key}\":\"\([^\"]*\)\".*/\1/p"
}

json_uint_text() {
    local text="$1"
    local key="$2"
    printf "%s\n" "$text" | sed -n "s/.*\"${key}\":\([0-9][0-9]*\).*/\1/p"
}

read_lease() {
    lease_json=$(bash "$lease_script" "$state_dir" read 2>/dev/null) || return 1
    generation=$(json_uint_text "$lease_json" generation)
    lease_state=$(json_string_text "$lease_json" state)
    arm_reason=$(json_string_text "$lease_json" arm_reason)
    arm_deadline=$(json_uint_text "$lease_json" arm_deadline)
    lease_ts=$(json_uint_text "$lease_json" ts)
    is_uint "$generation" && is_uint "$arm_deadline" && is_uint "$lease_ts" || return 1
    case "$lease_state" in
        active|armed|finalizing) return 0 ;;
        *) return 1 ;;
    esac
}

atomic_write() {
    local destination="$1"
    local contents="$2"
    local tmp="$state_dir/.$(basename "$destination").$$.$RANDOM.tmp"
    printf "%s\n" "$contents" > "$tmp" || return 1
    mv -f "$tmp" "$destination" || {
        rm -f "$tmp"
        return 1
    }
}

jupyter_is_idle() {
    local body compact without_idle
    body=$(curl -sS --max-time 5 -H "Authorization: token $token" \
        "http://127.0.0.1:$port/api/kernels" 2>/dev/null) || return 1
    compact=$(printf "%s" "$body" | tr -d " \n\r\t")
    case "$compact" in
        \[*\]) ;;
        *) return 1 ;;
    esac
    [ "$compact" = "[]" ] && return 0
    case "$compact" in
        *\"execution_state\":\"*) ;;
        *) return 1 ;;
    esac
    without_idle=$(printf "%s" "$compact" | sed "s/\"execution_state\":\"idle\"//g")
    case "$without_idle" in
        *\"execution_state\":\"*) return 1 ;;
        *) return 0 ;;
    esac
}

intent_action() {
    local compact
    decided_action="$default_action"
    [ -f "$state_dir/intent.json" ] || return 0
    intent=$(cat "$state_dir/intent.json" 2>/dev/null) || {
        decided_action="stop"
        return 0
    }
    compact=$(printf "%s" "$intent" | tr -d " \n\r\t")
    case "$compact" in
        *\"downloads_pending\":true*) downloads="true" ;;
        *\"downloads_pending\":false*) downloads="false" ;;
        *) downloads="" ;;
    esac
    requested=$(json_string_text "$compact" then)
    case "$downloads:$requested" in
        true:terminate) decided_action="stop" ;;
        true:stop|false:stop) decided_action="stop" ;;
        false:terminate) decided_action="terminate" ;;
        true:keep|false:keep) decided_action="keep" ;;
        *) decided_action="stop" ;;
    esac
}

run_finalize() {
    local limit="$1"
    local timeout_marker="$state_dir/.finalize-timeout.$$.$RANDOM"
    finalize_exit=0
    [ "$finalize_cmd" != "-" ] && [ -n "$finalize_cmd" ] || return 0
    if [ "$limit" -le 0 ]; then
        finalize_exit=124
        return 0
    fi

    "$finalize_cmd" &
    command_pid=$!
    (
        sleep "$limit"
        : > "$timeout_marker"
        kill -TERM "$command_pid" 2>/dev/null || exit 0
        sleep 1
        kill -KILL "$command_pid" 2>/dev/null || true
    ) &
    timer_pid=$!
    wait "$command_pid"
    command_status=$?
    kill "$timer_pid" 2>/dev/null || true
    wait "$timer_pid" 2>/dev/null || true
    if [ -f "$timeout_marker" ]; then
        rm -f "$timeout_marker"
        finalize_exit=124
    else
        finalize_exit="$command_status"
    fi
}

new_op_id() {
    hex=$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d " \n")
    if [ "${#hex}" -ne 32 ]; then
        hex=$(printf "%08x%08x%08x%08x" "$RANDOM" "$RANDOM" "$$" "$(date +%s)")
        hex=${hex:0:32}
    fi
    printf "%s-%s-%s-%s-%s\n" "${hex:0:8}" "${hex:8:4}" "${hex:12:4}" "${hex:16:4}" "${hex:20:12}"
}

remove_own_outcome() {
    [ -f "$outcome_path" ] || return 0
    current=$(cat "$outcome_path" 2>/dev/null) || return 0
    [ "$(json_string_text "$current" op_id)" = "$op_id" ] && rm -f "$outcome_path"
}

budget_is_due() {
    local deadline now
    [ -f "$state_dir/budget_deadline" ] || return 1
    deadline=$(cat "$state_dir/budget_deadline" 2>/dev/null) || return 1
    is_uint "$deadline" || return 1
    now=$(date +%s)
    [ "$now" -ge "$deadline" ]
}

pause_after_enter_for_test() {
    local delay="${RK_WATCHDOG_TEST_PAUSE_AFTER_ENTER_SECS:-}"
    [ -n "$delay" ] || return 0
    is_uint "$delay" || return 0
    if [ -n "${RK_WATCHDOG_TEST_PAUSE_AFTER_ENTER_MARKER:-}" ]; then
        : > "$RK_WATCHDOG_TEST_PAUSE_AFTER_ENTER_MARKER"
    fi
    sleep "$delay"
}

finish_armed() {
    local start_generation="$generation"
    local start_reason="$arm_reason"
    local start_ts="$lease_ts"
    local forced=0
    local existing_outcome=0

    if [ -e "$state_dir/outcome.json" ]; then
        if [ "$start_reason" = "budget" ]; then
            existing_outcome=1
        else
            printf "%s\n" "unresolved outcome.json blocks a new provider action" >&2
            return 0
        fi
    fi

    while true; do
        read_lease || return 0
        [ "$lease_state" = "armed" ] && [ "$arm_reason" = "$start_reason" ] || return 0
        if [ "$start_reason" = "disconnect" ] && [ "$generation" != "$start_generation" ]; then
            return 0
        fi
        if [ "$start_reason" = "disconnect" ] && budget_is_due; then
            return 0
        fi
        now=$(date +%s)
        if jupyter_is_idle; then
            break
        fi
        if [ "$start_reason" = "budget" ] && [ "$now" -ge "$arm_deadline" ]; then
            forced=1
            break
        fi
        if [ "$start_reason" = "disconnect" ] && [ "$finalize_wait_secs" -gt 0 ] \
            && [ "$now" -ge $((start_ts + finalize_wait_secs)) ]; then
            forced=1
            break
        fi
        sleep "$poll_secs"
    done

    intent_action
    if [ "$start_reason" = "budget" ] && { [ "$decided_action" = "keep" ] || [ "$existing_outcome" -eq 1 ]; }; then
        decided_action="stop"
    fi
    [ "$decided_action" != "keep" ] || return 0

    if [ "$start_reason" = "budget" ]; then
        now=$(date +%s)
        remaining=$((arm_deadline - now))
        if [ "$forced" -eq 1 ] || [ "$remaining" -le 0 ]; then
            decided_action="stop"
            finalize_exit=124
        else
            limit="$finalize_timeout_secs"
            [ "$limit" -le "$remaining" ] || limit="$remaining"
            run_finalize "$limit"
        fi
    else
        run_finalize "$finalize_timeout_secs"
    fi

    if [ "$finalize_exit" -ne 0 ] && [ "$decided_action" = "terminate" ]; then
        decided_action="stop"
    fi

    if [ "$start_reason" = "budget" ]; then
        while true; do
            read_lease || return 0
            [ "$lease_state" = "armed" ] && [ "$arm_reason" = "budget" ] || return 0
            now=$(date +%s)
            [ "$now" -ge "$arm_deadline" ] && break
            sleep "$poll_secs"
        done
        enter_authority="budget"
    else
        read_lease || return 0
        [ "$lease_state" = "armed" ] && [ "$arm_reason" = "disconnect" ] \
            && [ "$generation" = "$start_generation" ] || return 0
        enter_authority="$start_generation"
    fi

    op_id=$(new_op_id)
    action_ts=$(date +%s)
    if [ -e "$state_dir/outcome.json" ]; then
        if [ "$start_reason" != "budget" ]; then
            return 0
        fi
        existing_outcome=1
        decided_action="stop"
    fi
    if [ "$decided_action" = "terminate" ]; then
        post_action_rate=0
    else
        post_action_rate="$storage_rate_marker"
    fi
    if [ "$existing_outcome" -eq 1 ]; then
        outcome_path="$state_dir/outcome.$op_id.json"
    else
        outcome_path="$state_dir/outcome.json"
    fi
    outcome="{\"op_id\":\"$op_id\",\"action\":\"$decided_action\",\"finalize_exit\":$finalize_exit,\"ts\":$action_ts,\"generation\":$generation,\"post_action_rate\":$post_action_rate}"
    atomic_write "$outcome_path" "$outcome" || return 0

    bash "$lease_script" "$state_dir" enter-finalizing "$enter_authority" "$op_id" "$decided_action"
    enter_status=$?
    if [ "$enter_status" -eq "$EXIT_FENCED" ] || [ "$enter_status" -eq "$EXIT_REFUSED" ]; then
        remove_own_outcome
        return 0
    fi
    if [ "$enter_status" -ne 0 ]; then
        remove_own_outcome
        return 0
    fi

    sync "$outcome_path" 2>/dev/null || sync
    pause_after_enter_for_test
    exec "$action_cmd" "$decided_action"
}

supervise() {
    poll_secs="${RK_WATCHDOG_POLL_SECS:-2}"
    is_uint "$poll_secs" && [ "$poll_secs" -gt 0 ] || exit "$EXIT_INVALID"
    tracked_generation=""
    last_valid_touch=0

    while true; do
        if ! read_lease; then
            sleep "$poll_secs"
            continue
        fi
        now=$(date +%s)
        if [ "$generation" -eq 0 ] || [ "$lease_ts" -eq 0 ]; then
            tracked_generation="$generation"
            last_valid_touch="$now"
            sleep "$poll_secs"
            continue
        fi
        if [ "$tracked_generation" != "$generation" ]; then
            tracked_generation="$generation"
            last_valid_touch="$lease_ts"
        elif [ "$lease_ts" -gt "$last_valid_touch" ]; then
            last_valid_touch="$lease_ts"
        fi
        [ "$last_valid_touch" -le "$now" ] || last_valid_touch="$now"
        budget_due=0
        budget_is_due && budget_due=1

        if [ "$lease_state" = "active" ]; then
            if [ "$budget_due" -eq 1 ]; then
                bash "$lease_script" "$state_dir" arm "$generation" budget "$((now + grace_secs))" >/dev/null 2>&1 || true
            else
                if [ -f "$state_dir/heartbeat" ]; then
                    read -r heartbeat_gen heartbeat_ts extra < "$state_dir/heartbeat" || true
                    if is_uint "${heartbeat_gen:-}" && is_uint "${heartbeat_ts:-}" \
                        && [ -z "${extra:-}" ] && [ "$heartbeat_gen" = "$generation" ]; then
                        heartbeat_touch="$heartbeat_ts"
                        [ "$heartbeat_touch" -le "$now" ] || heartbeat_touch="$now"
                        if [ "$heartbeat_touch" -gt "$last_valid_touch" ]; then
                            last_valid_touch="$heartbeat_touch"
                        fi
                    fi
                fi
                if [ $((now - last_valid_touch)) -ge "$stale_secs" ]; then
                    bash "$lease_script" "$state_dir" arm "$generation" disconnect >/dev/null 2>&1 || true
                fi
            fi
        elif [ "$lease_state" = "armed" ]; then
            if [ "$budget_due" -eq 1 ] && [ "$arm_reason" != "budget" ]; then
                bash "$lease_script" "$state_dir" arm "$generation" budget "$((now + grace_secs))" >/dev/null 2>&1 || true
            else
                finish_armed
            fi
        else
            exit 0
        fi
        sleep "$poll_secs"
    done
}

[ "$#" -ge 2 ] || usage
state_dir="$1"
mode="$2"
shift 2

if [ "$mode" = "check-prereqs" ]; then
    [ "$#" -eq 0 ] || usage
    check_prereqs
    exit 0
fi

[ "$mode" = "install" ] || [ "$mode" = "supervise" ] || usage
[ "$#" -eq 10 ] || [ "$#" -eq 11 ] || usage
lease_script="$1"
stale_secs="$2"
grace_secs="$3"
finalize_wait_secs="$4"
finalize_timeout_secs="$5"
port="$6"
token="$7"
default_action="$8"
finalize_cmd="$9"
shift 9
action_cmd="$1"
shift
storage_rate_marker="${1:-null}"

is_uint "$stale_secs" && is_uint "$grace_secs" && is_uint "$finalize_wait_secs" \
    && is_uint "$finalize_timeout_secs" && is_uint "$port" || usage
[ "$finalize_timeout_secs" -gt 0 ] || usage
case "$default_action" in
    stop|terminate) ;;
    *) usage ;;
esac
[ "$storage_rate_marker" = "null" ] || is_rate "$storage_rate_marker" || usage
[ -f "$lease_script" ] && [ -n "$action_cmd" ] || usage
check_prereqs

if [ "$mode" = "supervise" ]; then
    supervise
    exit 0
fi

exec 8>"$state_dir/watchdog.lock" || exit "$EXIT_BAD_STATE_DIR"
if ! flock -n 8; then
    exit 0
fi

nohup bash "$0" "$state_dir" supervise "$lease_script" "$stale_secs" "$grace_secs" \
    "$finalize_wait_secs" "$finalize_timeout_secs" "$port" "$token" "$default_action" \
    "$finalize_cmd" "$action_cmd" "$storage_rate_marker" 8>&8 </dev/null >>"$state_dir/watchdog.log" 2>&1 &
watchdog_pid=$!
pid_tmp="$state_dir/.watchdog.pid.$$.$RANDOM.tmp"
printf "%s\n" "$watchdog_pid" > "$pid_tmp" && mv -f "$pid_tmp" "$state_dir/watchdog.pid"
exit 0
