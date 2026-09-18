#!/usr/bin/env bash
#
# The machine lock: one holder at a time for work that needs a quiet machine.
#
# The lock file lives in the repository's common git directory, so every
# worktree of this checkout resolves it to one path and one inode. `acquire`
# creates it under `set -C`, a single atomic open: of two callers racing, one
# wins and the other is told who holds it. `release` refuses unless the
# holder matches. `run` takes the lock, runs a command, and releases it
# whether the command succeeds, fails or is interrupted.
#
# A lock left behind by a run that died is cleared with `break`, which prints
# what it removed.
#
# Holding the lock says the machine is yours. It does not make the machine
# quiet, so `acquire` also reads what the machine is doing: a clickdoom
# container burning CPU refuses at once, and a load average above
# `MACHINE_LOCK_MAX_LOAD` is waited out before it refuses. `status` prints
# both beside the holder. `--force` takes the lock at once and needs a
# reason.
set -euo pipefail
cd "$(dirname "$0")/.."

lock="$(git rev-parse --path-format=absolute --git-common-dir)/machine-lock"

# The load average `acquire` refuses above. A setting that is not a number
# would compare as zero and refuse every acquire, so it is rejected here.
max_load="${MACHINE_LOCK_MAX_LOAD:-3}"
case "$max_load" in
    '' | *[!0-9.]* | *.*.*)
        echo "machine-lock: MACHINE_LOCK_MAX_LOAD is '$max_load', which is not a number" >&2
        exit 2
        ;;
esac

# The percentage of one CPU a clickdoom container may use and still count as
# idle. A ClickHouse with nothing asked of it reads a few percent, from its
# background merges.
max_container_cpu="${MACHINE_LOCK_MAX_CONTAINER_CPU:-10}"
case "$max_container_cpu" in
    '' | *[!0-9.]* | *.*.*)
        echo "machine-lock: MACHINE_LOCK_MAX_CONTAINER_CPU is '$max_container_cpu', which is not a number" >&2
        exit 2
        ;;
esac

# How long `acquire` waits for the load average to fall under the threshold
# before it refuses, in seconds. A handover straight after a full demo run
# was measured at 7.88 and took over two minutes to come down, so the
# default is longer than the minute the 1-minute average nominally takes.
settle="${MACHINE_LOCK_SETTLE:-180}"
case "$settle" in
    '' | *[!0-9]*)
        echo "machine-lock: MACHINE_LOCK_SETTLE is '$settle', which is not a whole number of seconds" >&2
        exit 2
        ;;
esac

# How often the wait looks again, in seconds.
settle_poll=5

# The holder `run` took the lock as, read back by the EXIT trap.
held_as=""

fail() { echo "machine-lock: $1" >&2; exit 1; }

usage() {
    cat >&2 <<'EOF'
usage: scripts/machine-lock.sh <command>

  status                              who holds it, and whether the machine is quiet
  path                                where the lock file is
  acquire [--force] <holder> [reason] take it, or report what is in the way and fail
  release <holder>                    give it back
  break                               clear a lock left by a run that died
  run [--force] <holder> <reason> -- <cmd>...   hold it for one command

MACHINE_LOCK_MAX_LOAD is the 1-minute load average acquire refuses above
(default 3), waited out for MACHINE_LOCK_SETTLE seconds (default 180) first.
MACHINE_LOCK_MAX_CONTAINER_CPU is the percentage of one CPU a clickdoom
container may use and still count as idle (default 10). --force takes the
lock at once through either refusal and needs a reason.
EOF
    exit 2
}

# The 1-minute load average as a decimal number, read wherever this host
# keeps it. Empty when none of the three sources answers.
load_average() {
    local reading
    if [ -r /proc/loadavg ]; then
        cut -d ' ' -f 1 /proc/loadavg
        return 0
    fi
    # macOS prints `{ 1.90 2.03 2.10 }`.
    reading="$(sysctl -n vm.loadavg 2>/dev/null || true)"
    if [ -n "$reading" ]; then
        echo "$reading" | awk '{ print $2 }'
        return 0
    fi
    uptime | sed 's/.*load average[s]*: *//' | tr -d ',' | awk '{ print $1 }'
}

# Every running container this repository's own work starts, as `name
# percent` per line. Empty when docker is absent or not answering, which
# leaves the reading to the load average alone.
#
# What a container is doing is the question, rather than whether it is
# there: a server left running after its own work finished costs the next
# holder almost nothing, and a list of names to forgive would grow with
# every lane.
clickdoom_container_cpu() {
    command -v docker >/dev/null 2>&1 || return 0
    docker stats --no-stream --format '{{.Name}} {{.CPUPerc}}' 2>/dev/null |
        grep '^clickdoom-' || true
}

# The ones working hard enough to be in the way, as `name at percent`.
busy_containers() {
    clickdoom_container_cpu | awk -v limit="$max_container_cpu" '
        { cpu = $2; sub(/%$/, "", cpu); if (cpu + 0 > limit) print $1 " at " $2 }
    '
}

# The two readings on one line, for `status` and for a refusal.
machine_state() {
    local load containers
    load="$(load_average)"
    containers="$(clickdoom_container_cpu | awk '{ printf "%s%s %s", sep, $1, $2; sep = ", " }')"
    echo "machine-lock: load ${load:-unknown} over 1 minute, \
containers ${containers:-none}"
}

status() {
    if [ -e "$lock" ]; then
        echo "machine-lock: held, $lock"
        cat "$lock"
    else
        echo "machine-lock: free, $lock"
    fi
    machine_state
}

# The `holder:` line's value. Empty when the lock is absent.
holder_of() {
    [ -e "$lock" ] || return 0
    sed -n 's/^holder: //p' "$lock"
}

# Whether the machine is quiet enough to take the lock. Prints what is in
# the way, one reason per line, and returns non-zero when it found any.
#
# A load average this cannot read is not treated as a reason, because a
# host that answers none of the three sources would refuse every acquire.
machine_is_quiet() {
    local load busy quiet=0
    busy="$(busy_containers | awk '{ printf "%s%s", sep, $0; sep = ", " }')"
    if [ -n "$busy" ]; then
        echo "  containers working: $busy"
        quiet=1
    fi
    load="$(load_average)"
    if [ -n "$load" ] && awk "BEGIN { exit !($load > $max_load) }"; then
        echo "  load $load is above MACHINE_LOCK_MAX_LOAD=$max_load"
        quiet=1
    fi
    return "$quiet"
}

# Waits for the load average to come down, and answers the way
# `machine_is_quiet` does: nothing on stdout and zero when the machine is
# quiet, the reasons on stdout and non-zero when it is not.
#
# Only the load is worth waiting for, because the 1-minute average keeps
# reading a run that has already ended. A container at work is not
# something a wait fixes, so that refuses at once. Everything is read
# again each time round, so a container that starts during the wait ends
# it.
wait_for_quiet() {
    local waited=0 blocking
    while :; do
        blocking="$(machine_is_quiet)" && return 0
        case "$blocking" in
            *'containers working:'*)
                echo "$blocking"
                return 1
                ;;
        esac
        if [ "$waited" -ge "$settle" ]; then
            echo "$blocking"
            echo "  waited ${settle}s (MACHINE_LOCK_SETTLE) for it to fall"
            return 1
        fi
        if [ "$waited" = 0 ]; then
            echo "machine-lock: waiting up to ${settle}s for the load to fall" >&2
        fi
        # purity-ok: pacing a wait for the machine to go quiet. No emulator
        # or benchmark result depends on it.
        sleep "$settle_poll"
        waited=$((waited + settle_poll))
    done
}

acquire() {
    local force=0
    if [ "${1-}" = "--force" ]; then
        force=1
        shift
    fi
    local holder="${1-}" reason="${2-}"
    [ -n "$holder" ] || fail "acquire needs a holder name"
    if [ "$force" = 1 ]; then
        [ -n "$reason" ] || fail "acquire --force needs a reason, since it takes the lock through a refusal"
    else
        local blocking
        if ! blocking="$(wait_for_quiet)"; then
            echo "machine-lock: the machine is not quiet:" >&2
            echo "$blocking" >&2
            echo "machine-lock: wait, or say why it is safe: \
scripts/machine-lock.sh acquire --force $holder '<reason>'" >&2
            exit 1
        fi
    fi
    # noclobber makes the redirect fail rather than truncate when the file is
    # already there, and that is what keeps two callers from both proceeding.
    if ! (
        set -C
        {
            echo "holder: $holder"
            # purity-ok: the lock records when a holder took it, for whoever
            # reads it back. No emulator or benchmark result depends on it.
            echo "started: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
            echo "host: $(uname -n)"
            echo "pid: $$"
            echo "worktree: $(pwd)"
            echo "reason: ${reason:-unstated}"
        } > "$lock"
    ) 2>/dev/null; then
        echo "machine-lock: already held, $lock" >&2
        cat "$lock" >&2
        echo "machine-lock: wait, or clear a dead holder with: scripts/machine-lock.sh break" >&2
        exit 1
    fi
    echo "machine-lock: taken by $holder"
}

release() {
    local holder="${1-}" current
    [ -n "$holder" ] || fail "release needs a holder name"
    [ -e "$lock" ] || fail "not held, so there is nothing to release ($lock)"
    current="$(holder_of)"
    [ "$current" = "$holder" ] || fail "held by $current, not $holder. Only the holder releases it"
    rm -f "$lock"
    echo "machine-lock: released by $holder"
}

break_lock() {
    if [ ! -e "$lock" ]; then
        echo "machine-lock: free already, $lock"
        return 0
    fi
    echo "machine-lock: breaking this lock:" >&2
    cat "$lock" >&2
    rm -f "$lock"
}

# Runs from `run`'s EXIT trap, so it reports nothing and changes no exit
# status. It removes the lock only if this call still holds it, which leaves
# a lock someone else has since taken alone.
release_on_exit() {
    [ -n "$held_as" ] || return 0
    [ -e "$lock" ] || return 0
    [ "$(holder_of)" = "$held_as" ] || return 0
    rm -f "$lock"
}

run() {
    local forced=()
    if [ "${1-}" = "--force" ]; then
        forced=(--force)
        shift
    fi
    local holder="${1-}" reason="${2-}"
    [ "$#" -ge 3 ] || fail "run needs <holder> <reason> -- <command>"
    shift 2
    [ "$1" = "--" ] || fail "run needs -- between the reason and the command"
    shift
    [ "$#" -gt 0 ] || fail "run needs a command after --"
    acquire "${forced[@]+"${forced[@]}"}" "$holder" "$reason"
    held_as="$holder"
    trap release_on_exit EXIT
    "$@"
}

case "${1-}" in
    status) status ;;
    path) echo "$lock" ;;
    acquire) shift; acquire "$@" ;;
    release) shift; release "$@" ;;
    break) break_lock ;;
    run) shift; run "$@" ;;
    *) usage ;;
esac
