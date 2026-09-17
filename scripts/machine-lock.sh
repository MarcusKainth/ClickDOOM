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
# quiet, so `acquire` also refuses while another lane's clickdoom container
# is running or the load average is above `MACHINE_LOCK_MAX_LOAD`, and
# `status` prints both beside the holder. `--force` takes it anyway and
# needs a reason.
set -euo pipefail
cd "$(dirname "$0")/.."

lock="$(git rev-parse --path-format=absolute --git-common-dir)/machine-lock"

# The compose service in docker-compose.yml. It sits on the development
# machine all day, so it is not a reason to refuse.
shared_container="clickdoom-ch"

# The load average `acquire` refuses above. A setting that is not a number
# would compare as zero and refuse every acquire, so it is rejected here.
max_load="${MACHINE_LOCK_MAX_LOAD:-3}"
case "$max_load" in
    '' | *[!0-9.]* | *.*.*)
        echo "machine-lock: MACHINE_LOCK_MAX_LOAD is '$max_load', which is not a number" >&2
        exit 2
        ;;
esac

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
(default 3). --force takes the lock through either refusal and needs a reason.
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

# Every running container this repository's own work starts, one name per
# line. Empty when docker is absent or not answering, which leaves the
# reading to the load average alone.
clickdoom_containers() {
    command -v docker >/dev/null 2>&1 || return 0
    docker ps --filter 'name=^clickdoom-' --format '{{.Names}}' 2>/dev/null || true
}

# The ones in the way of `holder` taking the lock: every running container
# but the shared server and the holder's own, which a holder's own timing
# run needs up and may have started either side of taking the lock.
#
# A holder names itself either way round: `MACHINE_LOCK_HOLDER` defaults to
# `$USER`, whose container is `clickdoom-$USER`, and a lane takes the lock
# under its container's own name. Both spellings are the holder's own.
competing_containers() {
    local holder="$1"
    clickdoom_containers |
        grep -vxF -e "$shared_container" -e "$holder" -e "clickdoom-$holder" || true
}

# The two readings on one line, for `status` and for a refusal.
machine_state() {
    local load containers
    load="$(load_average)"
    containers="$(clickdoom_containers | tr '\n' ' ')"
    containers="${containers% }"
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
# Another lane's container is in the way whatever it is doing: an idle
# server still holds the memory and the threads a timing would otherwise
# have. A load average this cannot read is not treated as a reason,
# because a host that answers none of the three sources would refuse every
# acquire.
machine_is_quiet() {
    local holder="$1" load containers quiet=0
    containers="$(competing_containers "$holder" | tr '\n' ' ')"
    if [ -n "${containers% }" ]; then
        echo "  containers running: ${containers% }"
        quiet=1
    fi
    load="$(load_average)"
    if [ -n "$load" ] && awk "BEGIN { exit !($load > $max_load) }"; then
        echo "  load $load is above MACHINE_LOCK_MAX_LOAD=$max_load"
        quiet=1
    fi
    return "$quiet"
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
        if ! blocking="$(machine_is_quiet "$holder")"; then
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
