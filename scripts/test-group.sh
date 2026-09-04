#!/usr/bin/env bash
#
# Runs one group of the test suites, the way ci.yml runs them in parallel.
#
#   scripts/test-group.sh <group>
#
# Groups, and what they hold:
#   emulator       every suite outside the native crate and the driver's
#                  native_* suites: the SQL CPU, the executor, the reference
#                  emulator and the driver's emulation side, then the ROM
#                  suites that need a release build
#   native-sim-a   the tic, plat and compaction suites
#   native-sim-b   every simulation suite the other groups do not name
#   native-sim-c   the hearing, missile, parity and move suites
#   native-sim-d   the shot and fall suites
#   native-sim-e   the thrust suite
#   native-rest    the native crate's loader, renderer and table suites
#   driver-native  the driver's native_* suites: load, render, demo, play,
#                  diff, session and stream; the connection suite runs in
#                  `emulator`, where nothing runs beside it
#
# Every group but `emulator` needs a reachable ClickHouse
# (CLICKHOUSE_HOST/CLICKHOUSE_HTTP_PORT/CLICKHOUSE_PASSWORD); `emulator`
# needs that plus rom/build and target/release/refemu. `make test` runs the
# same suites in one pass without nextest.
set -euo pipefail
cd "$(dirname "$0")/.."

group="${1-}"

# With NEXTEST_ARCHIVE_DIR set, the suites come pre-built from
# `cargo nextest archive` (tests.tar.zst for the workspace with the live
# suites, rom-suites.tar.zst for the reference emulator's release ROM
# suites) and nothing is compiled here. Without it, each run builds what it
# needs.
archive="${NEXTEST_ARCHIVE_DIR-}"
if [ -n "$archive" ]; then
    # Extracted over the workspace rather than into a temporary directory:
    # a test that runs the driver binary reaches it by the path compiled in
    # at build time, which is the workspace's own target directory.
    extract=(--workspace-remap . --extract-to . --extract-overwrite)
    run() {
        cargo nextest run --archive-file "$archive/tests.tar.zst" "${extract[@]}" "$@"
    }
    run_rom() {
        cargo nextest run --archive-file "$archive/rom-suites.tar.zst" "${extract[@]}" "$@"
    }
    live=""
else
    run() { cargo nextest run --locked "$@"; }
    run_rom() { cargo nextest run --locked --release -p refemu --features rom-tests "$@"; }
    live="--workspace --features clickhouse-tests"
fi

case "$group" in
    emulator)
        # One test at a time: the SQL CPU's suite and the executor's share
        # the server's compiled-expression cache, which a second run beside
        # them would warm or cool, and the connection suite counts the
        # server's connections, which a neighbour's session would move.
        # shellcheck disable=SC2086 # $live is a list of flags or nothing
        run $live --test-threads 1 \
            -E 'not package(clickdoom-native) and (not binary(/^native_/) or binary(native_connections_live))'
        # The ROM suites are the reference emulator's, so only it is built
        # in release.
        run_rom \
            -E 'binary(reference_trace) | binary(demo3_parity) | binary(rom_symbols) | binary(probe_fixture)'
        ;;
    # A test that opens a session pays the tic statement's analysis, about
    # five minutes on a runner, and the analysis runs on one thread, so the
    # simulation groups run four tests at a time and the suites that open
    # sessions are spread over the groups by their measured length.
    native-sim-a)
        # shellcheck disable=SC2086
        run $live --test-threads 4 \
            -E 'package(clickdoom-native) and (binary(sim_tic_live) | binary(sim_plat_live) | binary(sim_compact_live))'
        ;;
    native-sim-b)
        # shellcheck disable=SC2086
        run $live --test-threads 4 \
            -E 'package(clickdoom-native) and binary(/^sim_/) and not (binary(sim_tic_live) | binary(sim_plat_live) | binary(sim_compact_live) | binary(sim_hearing_live) | binary(sim_missile_live) | binary(sim_parity_live) | binary(sim_move_live) | binary(sim_shot_live) | binary(sim_fall_live) | binary(sim_thrust_live))'
        ;;
    native-sim-c)
        # shellcheck disable=SC2086
        run $live --test-threads 4 \
            -E 'package(clickdoom-native) and (binary(sim_hearing_live) | binary(sim_missile_live) | binary(sim_parity_live) | binary(sim_move_live))'
        ;;
    native-sim-d)
        # shellcheck disable=SC2086
        run $live --test-threads 4 \
            -E 'package(clickdoom-native) and (binary(sim_shot_live) | binary(sim_fall_live))'
        ;;
    native-sim-e)
        # shellcheck disable=SC2086
        run $live --test-threads 4 \
            -E 'package(clickdoom-native) and binary(sim_thrust_live)'
        ;;
    native-rest)
        # shellcheck disable=SC2086
        run $live --test-threads 2 \
            -E 'package(clickdoom-native) and not binary(/^sim_/)'
        ;;
    driver-native)
        # shellcheck disable=SC2086
        run $live --test-threads 2 \
            -E 'package(clickdoom-driver) and binary(/^native_/) and not binary(native_connections_live)'
        ;;
    *)
        echo "usage: scripts/test-group.sh emulator|native-sim-a|native-sim-b|native-sim-c|native-sim-d|native-sim-e|native-rest|driver-native" >&2
        exit 2
        ;;
esac
