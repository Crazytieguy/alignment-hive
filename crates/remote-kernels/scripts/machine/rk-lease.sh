#!/usr/bin/env bash
# Three-state fenced lease: active -> armed -> finalizing. Every read/check/write
# is serialized by lease.lock; lease.json is replaced atomically. Exit codes:
# 0 success, 9 fenced generation/operation, 10 finalizing refusal, 11 bad input
# or corrupt state. This file contains no single quotes so it can later be
# embedded in the existing single-quote-wrapped SSH transport. A numeric
# enter-finalizing authority is an explicit live-owner operation and may
# override active, disconnect-armed, or budget-armed state.

set -u
umask 077

readonly EXIT_FENCED=9
readonly EXIT_REFUSED=10
readonly EXIT_INVALID=11

fail_invalid() {
    printf "%s\n" "$*" >&2
    exit "$EXIT_INVALID"
}

is_uint() {
    case "${1:-}" in
        ""|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

is_atom() {
    case "${1:-}" in
        ""|*[!A-Za-z0-9._:@+-]*) return 1 ;;
        *) return 0 ;;
    esac
}

json_string() {
    local key="$1"
    sed -n "s/.*\"${key}\":\"\([^\"]*\)\".*/\1/p" "$lease_file"
}

json_uint() {
    local key="$1"
    sed -n "s/.*\"${key}\":\([0-9][0-9]*\).*/\1/p" "$lease_file"
}

# A finalizing lease binds only while the process that entered it can still
# finish it: the watchdog recorded in watchdog.pid on THIS machine. The state
# directory may live on a persistent volume (RunPod network volumes are
# mounted at the workdir), so a pod that died mid-finalize leaves a finalizing
# lease that every later pod mounting the volume would otherwise inherit as
# "running its automatic cleanup" -- with no process anywhere able to clear
# it. No live finalizer here means the lease is a fossil, not an operation.
finalizer_alive() {
    local pid_file="$state_dir/watchdog.pid" pid=""
    [ -f "$pid_file" ] || return 1
    pid=$(tr -cd "0-9" < "$pid_file")
    is_uint "$pid" || return 1
    kill -0 "$pid" 2>/dev/null || return 1
    tr "\0" " " < "/proc/$pid/cmdline" 2>/dev/null | grep -q "rk-watchdog"
}

load_lease() {
    if [ ! -f "$lease_file" ]; then
        generation=0
        owner_uuid=""
        state="active"
        arm_reason=""
        arm_deadline=0
        op_id=""
        action=""
        lease_ts=0
        return 0
    fi

    generation=$(json_uint generation)
    owner_uuid=$(json_string owner_uuid)
    state=$(json_string state)
    arm_reason=$(json_string arm_reason)
    arm_deadline=$(json_uint arm_deadline)
    op_id=$(json_string op_id)
    action=$(json_string action)
    lease_ts=$(json_uint ts)

    is_uint "$generation" && is_uint "$arm_deadline" && is_uint "$lease_ts" \
        || fail_invalid "corrupt lease: invalid numeric field"
    case "$state" in
        active|armed|finalizing) ;;
        *) fail_invalid "corrupt lease: invalid state" ;;
    esac
    case "$arm_reason" in
        ""|disconnect|budget) ;;
        *) fail_invalid "corrupt lease: invalid arm reason" ;;
    esac
    if [ "$state" = "armed" ]; then
        [ -n "$arm_reason" ] || fail_invalid "corrupt lease: armed state has no reason"
    fi
    lease_raw=$(cat "$lease_file")
    expected=$(printf "{\"generation\":%s,\"owner_uuid\":\"%s\",\"state\":\"%s\",\"arm_reason\":\"%s\",\"arm_deadline\":%s,\"op_id\":\"%s\",\"action\":\"%s\",\"ts\":%s}" \
        "$generation" "$owner_uuid" "$state" "$arm_reason" "$arm_deadline" \
        "$op_id" "$action" "$lease_ts")
    [ "$lease_raw" = "$expected" ] || fail_invalid "corrupt lease: unexpected shape"
}

pause_after_read_for_test() {
    local delay="${RK_LEASE_TEST_PAUSE_AFTER_READ_SECS:-}"
    [ -n "$delay" ] || return 0
    is_uint "$delay" || fail_invalid "invalid test pause"
    if [ -n "${RK_LEASE_TEST_PAUSE_MARKER:-}" ]; then
        : > "$RK_LEASE_TEST_PAUSE_MARKER"
    fi
    sleep "$delay"
}

write_lease() {
    local now tmp
    now=$(date +%s)
    lease_ts="$now"
    tmp="$state_dir/.lease.json.$$.$RANDOM.tmp"
    printf "{\"generation\":%s,\"owner_uuid\":\"%s\",\"state\":\"%s\",\"arm_reason\":\"%s\",\"arm_deadline\":%s,\"op_id\":\"%s\",\"action\":\"%s\",\"ts\":%s}\n" \
        "$generation" "$owner_uuid" "$state" "$arm_reason" "$arm_deadline" \
        "$op_id" "$action" "$now" > "$tmp" || fail_invalid "cannot write lease"
    mv -f "$tmp" "$lease_file" || {
        rm -f "$tmp"
        fail_invalid "cannot replace lease"
    }
}

print_lease() {
    local now
    now=$(date +%s)
    printf "{\"generation\":%s,\"owner_uuid\":\"%s\",\"state\":\"%s\",\"arm_reason\":\"%s\",\"arm_deadline\":%s,\"op_id\":\"%s\",\"action\":\"%s\",\"ts\":%s,\"now\":%s}\n" \
        "$generation" "$owner_uuid" "$state" "$arm_reason" "$arm_deadline" \
        "$op_id" "$action" "$lease_ts" "$now"
}

[ "$#" -ge 2 ] || fail_invalid "usage: rk-lease.sh <state_dir> <op> [args]"
state_dir="$1"
op="$2"
shift 2

command -v flock >/dev/null 2>&1 || fail_invalid "flock is required"
mkdir -p "$state_dir" || fail_invalid "cannot create state directory"
lease_file="$state_dir/lease.json"
lock_file="$state_dir/lease.lock"

exec 9>"$lock_file" || fail_invalid "cannot open lease lock"
flock -x 9 || fail_invalid "cannot lock lease"
load_lease

case "$op" in
    acquire)
        [ "$#" -eq 1 ] && is_atom "$1" || fail_invalid "usage: acquire <owner_uuid>"
        if [ "$state" = "finalizing" ]; then
            finalizer_alive && exit "$EXIT_REFUSED"
            # Stale finalize from a machine that no longer exists: its
            # outcome marker would block the finalize of this machine.
            rm -f "$state_dir/outcome.json" "$state_dir/watchdog.pid"
            printf "%s\n" "reclaimed a finalizing lease with no live finalizer (op ${op_id:-?}, action ${action:-?})" >&2
        fi
        pause_after_read_for_test
        generation=$((generation + 1))
        owner_uuid="$1"
        op_id=""
        if [ "$state" = "armed" ] && [ "$arm_reason" = "budget" ]; then
            :
        else
            state="active"
            arm_reason=""
            arm_deadline=0
            action=""
        fi
        write_lease
        print_lease
        ;;
    refresh)
        [ "$#" -eq 2 ] && is_uint "$1" && is_atom "$2" \
            || fail_invalid "usage: refresh <gen> <owner_uuid>"
        [ "$state" != "finalizing" ] || exit "$EXIT_REFUSED"
        [ "$generation" = "$1" ] && [ "$owner_uuid" = "$2" ] || exit "$EXIT_FENCED"
        pause_after_read_for_test
        if [ "$state" = "armed" ] && [ "$arm_reason" = "disconnect" ]; then
            state="active"
            arm_reason=""
            arm_deadline=0
            op_id=""
            action=""
        fi
        write_lease
        ;;
    arm)
        [ "$#" -ge 2 ] && [ "$#" -le 3 ] && is_uint "$1" \
            || fail_invalid "usage: arm <gen> <disconnect|budget> [abs_deadline_epoch]"
        [ "$state" != "finalizing" ] || exit "$EXIT_REFUSED"
        [ "$generation" = "$1" ] || exit "$EXIT_FENCED"
        case "$2" in
            disconnect)
                if [ "$#" -eq 3 ]; then
                    is_uint "$3" || fail_invalid "invalid disconnect deadline"
                    requested_deadline="$3"
                else
                    requested_deadline=0
                fi
                ;;
            budget)
                [ "$#" -eq 3 ] && is_uint "$3" \
                    || fail_invalid "budget arm requires an absolute deadline"
                requested_deadline="$3"
                ;;
            *) fail_invalid "arm reason must be disconnect or budget" ;;
        esac
        pause_after_read_for_test
        if [ "$state" = "armed" ] && [ "$arm_reason" = "budget" ]; then
            :
        else
            state="armed"
            arm_reason="$2"
            arm_deadline="$requested_deadline"
            op_id=""
        fi
        write_lease
        ;;
    enter-finalizing)
        [ "$#" -eq 3 ] && is_atom "$2" \
            || fail_invalid "usage: enter-finalizing <gen|budget> <op_id> <action>"
        case "$3" in
            stop|terminate) ;;
            *) fail_invalid "finalizing action must be stop or terminate" ;;
        esac
        [ "$state" != "finalizing" ] || exit "$EXIT_REFUSED"
        if [ "$1" = "budget" ]; then
            now=$(date +%s)
            [ "$state" = "armed" ] && [ "$arm_reason" = "budget" ] \
                && [ "$now" -ge "$arm_deadline" ] || exit "$EXIT_FENCED"
        else
            is_uint "$1" || fail_invalid "invalid generation"
            [ "$generation" = "$1" ] || exit "$EXIT_FENCED"
            [ "$state" = "active" ] || [ "$state" = "armed" ] || exit "$EXIT_FENCED"
        fi
        pause_after_read_for_test
        state="finalizing"
        op_id="$2"
        action="$3"
        write_lease
        ;;
    revert-to-armed)
        [ "$#" -eq 1 ] && is_atom "$1" || fail_invalid "usage: revert-to-armed <op_id>"
        [ "$state" = "finalizing" ] || fail_invalid "lease is not finalizing"
        [ "$op_id" = "$1" ] || exit "$EXIT_FENCED"
        pause_after_read_for_test
        if [ -n "$arm_reason" ]; then
            state="armed"
        else
            state="active"
        fi
        op_id=""
        action=""
        write_lease
        ;;
    complete-stop)
        [ "$#" -eq 1 ] && is_atom "$1" || fail_invalid "usage: complete-stop <op_id>"
        [ "$state" = "finalizing" ] && [ "$op_id" = "$1" ] && [ "$action" = "stop" ] \
            || exit "$EXIT_FENCED"
        pause_after_read_for_test
        state="active"
        owner_uuid=""
        arm_reason=""
        arm_deadline=0
        op_id=""
        action=""
        write_lease
        ;;
    read)
        [ "$#" -eq 0 ] || fail_invalid "usage: read"
        print_lease
        ;;
    *) fail_invalid "unknown lease operation: $op" ;;
esac
