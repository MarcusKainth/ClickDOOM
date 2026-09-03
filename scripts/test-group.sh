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
#   native-sim-a   the three longest native simulation suites
#   native-sim-b   the other native simulation suites
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
run() { cargo nextest run --locked "$@"; }
live="--workspace --features clickhouse-tests"

case "$group" in
    emulator)
        # One test at a time: the SQL CPU's suite and the executor's share
        # the server's compiled-expression cache, which a second run beside
        # them would warm or cool, and the connection suite counts the
        # server's connections, which a neighbour's session would move.
        run $live --test-threads 1 \
            -E 'not package(clickdoom-native) and (not binary(/^native_/) or binary(native_connections_live))'
        run --release --workspace --features refemu/rom-tests \
            -E 'binary(reference_trace) | binary(demo3_parity) | binary(rom_symbols) | binary(probe_fixture)'
        ;;
    native-sim-a)
        run $live --test-threads 2 \
            -E 'package(clickdoom-native) and (binary(sim_tic_live) | binary(sim_plat_live) | binary(sim_compact_live))'
        ;;
    native-sim-b)
        run $live --test-threads 2 \
            -E 'package(clickdoom-native) and binary(/^sim_/) and not (binary(sim_tic_live) | binary(sim_plat_live) | binary(sim_compact_live))'
        ;;
    native-rest)
        run $live --test-threads 2 \
            -E 'package(clickdoom-native) and not binary(/^sim_/)'
        ;;
    driver-native)
        run $live --test-threads 2 \
            -E 'package(clickdoom-driver) and binary(/^native_/) and not binary(native_connections_live)'
        ;;
    *)
        echo "usage: scripts/test-group.sh emulator|native-sim-a|native-sim-b|native-rest|driver-native" >&2
        exit 2
        ;;
esac
