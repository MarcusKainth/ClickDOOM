#!/usr/bin/env bash
#
# Runs one group of the test suites, the way ci.yml runs them in parallel.
#
#   scripts/test-group.sh <group>
#   scripts/test-group.sh --list <group>
#   scripts/test-group.sh --check
#
# --list prints the tests a group selects, one `<binary-id> <test>` per
# line, and runs nothing. --check lists every group and every test the
# suites hold, and fails when a test is in no group or in two, or when a
# group selects nothing.
#
# Groups, and what they hold:
#   emulator       every suite outside the native crate and the driver's
#                  native_* suites: the SQL CPU, the executor, the reference
#                  emulator and the driver's emulation side, then the ROM
#                  suites that need a release build
#   native-sim-a   the compact, missile, missile-wall, player-frames and
#                  refire suites
#   native-sim-b   the door, missile-kill-drop, move and player-damage
#                  suites
#   native-sim-c   the gunshot-kill-drop, pain, punch, shot, thinker-
#                  order and troop suites
#   native-sim-d   the floor, hearing, justattacked, lights, thrust
#                  and tic suites
#   native-sim-e   the blast, claw, fall, gunshot, hitscan, input,
#                  kills-and-drops, missile-same-target, setup, spawn,
#                  throw, traverse and use suites
#   native-sim-f   the aim, damage, impact, noise, parity, plat,
#                  removed and sight suites
#   native-rest    everything native outside the simulation: the native
#                  crate's loader, renderer and table suites, and the
#                  driver's native_* suites (load, render, demo, play, diff,
#                  session and stream). The connection suite runs in
#                  `emulator`, where nothing runs beside it
#
# Every group but `emulator` needs a reachable ClickHouse
# (CLICKHOUSE_HOST/CLICKHOUSE_HTTP_PORT/CLICKHOUSE_PASSWORD); `emulator`
# needs that plus rom/build and target/release/refemu. `make test` runs the
# same suites in one pass without nextest.
set -euo pipefail
cd "$(dirname "$0")/.."

groups=(emulator native-sim-a native-sim-b native-sim-c native-sim-d native-sim-e native-sim-f native-rest)

verb=run
if [ "${1-}" = --list ]; then
    verb=list
    shift
fi
group="${1-}"

# `cargo nextest run` with the arguments given, or under --list
# `cargo nextest list` with the same selection.
nextest() {
    if [ "$verb" = run ]; then
        cargo nextest run "$@"
        return
    fi
    local selection=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --test-threads) shift 2 ;;
            *) selection+=("$1"); shift ;;
        esac
    done
    cargo nextest list --message-format oneline "${selection[@]}"
}

# With NEXTEST_ARCHIVE_DIR set, the suites come pre-built from
# `cargo nextest archive` and nothing is compiled here: native.tar.zst holds
# the native crate's suites, workspace.tar.zst every other crate's with the
# live suites, and rom-suites.tar.zst the reference emulator's release ROM
# suites. Each group names the archives it runs from. Without it, each run
# builds what it needs.
archive="${NEXTEST_ARCHIVE_DIR-}"
if [ -n "$archive" ]; then
    # Extracted over the workspace rather than into a temporary directory:
    # a test that runs the driver binary reaches it by the path compiled in
    # at build time, which is the workspace's own target directory.
    extract=(--workspace-remap . --extract-to . --extract-overwrite)
    run_native() {
        nextest --archive-file "$archive/native.tar.zst" "${extract[@]}" "$@"
    }
    run_workspace() {
        nextest --archive-file "$archive/workspace.tar.zst" "${extract[@]}" "$@"
    }
    run_rom() {
        nextest --archive-file "$archive/rom-suites.tar.zst" "${extract[@]}" "$@"
    }
    live=""
else
    run_native() { nextest --locked "$@"; }
    run_workspace() { nextest --locked "$@"; }
    run_rom() { nextest --locked --release -p refemu --features rom-tests "$@"; }
    live="--workspace --features clickhouse-tests"
fi

# The simulation suites the lettered groups name, one per line so a move
# between groups is one line of diff. native-sim-e is every simulation suite
# not listed here, so a suite added to a lettered group has to be added here
# in the same commit or it runs twice, and a suite added to none of them runs
# in native-sim-e rather than in nothing.
#
# The packing is over each group's own thread schedule, longest test first
# over TEST_THREADS, taken on the median of every test across five main
# runs. A group's cost is not its total: a test cannot be split, so a group
# holding one very long test costs at least that test however little else it
# carries. Five of the suites here are a single test of 14 to 21 minutes.
#
# native-sim-e, by name: sim_blast_live, sim_claw_live, sim_fall_live, sim_gunshot_live,
# sim_hitscan_live, sim_input_live, sim_kills_and_drops_live,
# sim_missile_same_target_live, sim_setup_live, sim_spawn_live,
# sim_throw_live, sim_traverse_live, sim_use_live.
sim_a='binary(sim_compact_live) | binary(sim_missile_live) | binary(sim_missile_wall_live) | binary(sim_player_frames_live) | binary(sim_refire_live)'
sim_b='binary(sim_door_live) | binary(sim_missile_kill_drop_live) | binary(sim_move_live) | binary(sim_player_damage_live)'
sim_c='binary(sim_gunshot_kill_drop_live) | binary(sim_pain_live) | binary(sim_punch_live) | binary(sim_shot_live) | binary(sim_thinker_order_live) | binary(sim_troop_live)'
sim_d='binary(sim_floor_live) | binary(sim_hearing_live) | binary(sim_justattacked_live) | binary(sim_lights_live) | binary(sim_thrust_live) | binary(sim_tic_live)'
sim_f='binary(sim_aim_live) | binary(sim_damage_live) | binary(sim_impact_live) | binary(sim_noise_live) | binary(sim_parity_live) | binary(sim_plat_live) | binary(sim_removed_live) | binary(sim_sight_live)'

case "$group" in
    --check)
        listed=$(mktemp -d)
        trap 'rm -rf "$listed"' EXIT
        verb=list
        # shellcheck disable=SC2086
        { run_native $live; run_workspace $live; run_rom; } | sort -u > "$listed/all"
        failed=0
        for g in "${groups[@]}"; do
            "$0" --list "$g" | sort -u | sed "s|^|$g |" > "$listed/group-$g"
            echo "$g: $(wc -l < "$listed/group-$g") tests"
            if [ ! -s "$listed/group-$g" ]; then
                echo "::error::$g selects no tests" >&2
                failed=1
            fi
        done
        cat "$listed"/group-* | cut -d' ' -f2- | sort > "$listed/selected"
        echo "every test: $(wc -l < "$listed/all"); selected: $(wc -l < "$listed/selected")"
        if uniq -d "$listed/selected" | grep .; then
            echo "::error::the tests above are selected by more than one group" >&2
            failed=1
        fi
        if comm -23 "$listed/all" <(sort -u "$listed/selected") | grep .; then
            echo "::error::the tests above are selected by no group" >&2
            failed=1
        fi
        exit "$failed"
        ;;
    emulator)
        # One test at a time: the SQL CPU's suite and the executor's share
        # the server's compiled-expression cache, which a second run beside
        # them would warm or cool, and the connection suite counts the
        # server's connections, which a neighbour's session would move.
        # shellcheck disable=SC2086 # $live is a list of flags or nothing
        run_workspace $live --test-threads 1 \
            -E 'not package(clickdoom-native) and (not binary(/^native_/) or binary(native_connections_live))'
        # The ROM suites are the reference emulator's, so only it is built
        # in release.
        run_rom \
            -E 'binary(reference_trace) | binary(demo3_parity) | binary(rom_symbols) | binary(probe_fixture)'
        ;;
    # A test that opens a session pays the tic statement's analysis, about
    # three minutes on a standard runner, and the analysis runs on one
    # thread, so the simulation groups run TEST_THREADS tests at a time
    # (four unless set, which a standard runner's four cores fill) and the
    # suites that open sessions are spread over the groups by their
    # measured length. A runner with more cores sets TEST_THREADS higher.
    native-sim-a)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and ($sim_a)"
        ;;
    native-sim-b)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and ($sim_b)"
        ;;
    native-sim-c)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and ($sim_c)"
        ;;
    native-sim-d)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and ($sim_d)"
        ;;
    native-sim-e)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and binary(/^sim_/) and not ($sim_a | $sim_b | $sim_c | $sim_d | $sim_f)"
        ;;
    native-sim-f)
        # shellcheck disable=SC2086
        run_native $live --test-threads "${TEST_THREADS:-4}" \
            -E "package(clickdoom-native) and ($sim_f)"
        ;;
    native-rest)
        # shellcheck disable=SC2086
        run_native $live --test-threads 2 \
            -E 'package(clickdoom-native) and not binary(/^sim_/)'
        # shellcheck disable=SC2086
        run_workspace $live --test-threads 2 \
            -E 'package(clickdoom-driver) and binary(/^native_/) and not binary(native_connections_live)'
        ;;
    *)
        echo "usage: scripts/test-group.sh [--list] $(IFS='|'; echo "${groups[*]}") | --check" >&2
        exit 2
        ;;
esac
