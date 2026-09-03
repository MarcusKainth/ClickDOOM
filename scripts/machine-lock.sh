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
set -euo pipefail
cd "$(dirname "$0")/.."

lock="$(git rev-parse --path-format=absolute --git-common-dir)/machine-lock"

# The holder `run` took the lock as, read back by the EXIT trap.
held_as=""

fail() { echo "machine-lock: $1" >&2; exit 1; }

usage() {
    cat >&2 <<'EOF'
usage: scripts/machine-lock.sh <command>

  status                              who holds it, or that it is free
  path                                where the lock file is
  acquire <holder> [reason]           take it, or report the holder and fail
  release <holder>                    give it back
  break                               clear a lock left by a run that died
  run <holder> <reason> -- <cmd>...   hold it for one command
EOF
    exit 2
}

status() {
    if [ -e "$lock" ]; then
        echo "machine-lock: held, $lock"
        cat "$lock"
    else
        echo "machine-lock: free, $lock"
    fi
}

# The `holder:` line's value. Empty when the lock is absent.
holder_of() {
    [ -e "$lock" ] || return 0
    sed -n 's/^holder: //p' "$lock"
}

acquire() {
    local holder="${1-}" reason="${2-}"
    [ -n "$holder" ] || fail "acquire needs a holder name"
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
    local holder="${1-}" reason="${2-}"
    [ "$#" -ge 3 ] || fail "run needs <holder> <reason> -- <command>"
    shift 2
    [ "$1" = "--" ] || fail "run needs -- between the reason and the command"
    shift
    [ "$#" -gt 0 ] || fail "run needs a command after --"
    acquire "$holder" "$reason"
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
