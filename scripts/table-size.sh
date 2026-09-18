#!/usr/bin/env bash
#
# Does a tic cost more when the state tables hold more rows?
#
#   scripts/table-size.sh <port>
#
# The world is held still and only the table size moves. Every run loads
# the level, walks to tic 20, and then pads `native_state` and
# `native_stage` with rows at tic keys nothing reads. So each measurement
# does the same tic's work over the same world, against tables of
# different sizes, and a difference between them is the size and nothing
# else. Walking further instead would grow the tables and the world
# together and could not tell them apart.
#
# Reports, for each size, the driver's own presence lookup and the whole
# first statement, each as wall time and as CPU. Wall time on a busy
# server is mostly waiting, so the two columns answer different questions
# and the CPU one is what says whether work grew.
#
# Needs `cargo test -p clickdoom-native --features clickhouse-tests --test
# dump_stage1` to have written target/stage1_tic21.sql, and a ClickHouse
# on <port> with nothing else running against it.
#
# WORK IN PROGRESS. The numbers this produces have not been taken yet.
set -euo pipefail
cd "$(dirname "$0")/.."

port="${1-18125}"
statement="target/stage1_tic21.sql"
probe="refemu/reference_traces/demo3/probe.9a6a47d01119.tsv"
ch="http://localhost:${port}/?user=default&password=clickdoom&database=clickdoom"

test -f "$statement" || {
    echo "$statement missing. Run: cargo test -p clickdoom-native \\" >&2
    echo "  --features clickhouse-tests --test dump_stage1" >&2
    exit 1
}

q() { curl -s "$ch" --data-binary "$1"; }

# The average wall time and CPU of every query since $1 whose text holds $2.
spent() {
    q "SELECT round(avg(query_duration_ms), 3), \
              round(avg(ProfileEvents['UserTimeMicroseconds']) / 1000, 3) \
       FROM system.query_log \
       WHERE event_time >= '$1' AND type = 'QueryFinish' \
         AND position(query, '$2') > 0 FORMAT TSV"
}

printf 'rows\tstate\tstage\tlookup_ms\tlookup_cpu\tstage1_ms\tstage1_cpu\n'
for pad in 0 50 200 800 2000 8000; do
    CLICKHOUSE_PASSWORD=clickdoom ./target/release/clickdoom native load \
        --fresh --port "$port" --wad rom/wad/doom1.wad >/dev/null 2>&1
    CLICKHOUSE_PASSWORD=clickdoom ./target/release/clickdoom native diff 21 \
        --port "$port" --probe "$probe" >/dev/null 2>&1 || true
    if [ "$pad" -gt 0 ]; then
        for table in native_state native_stage; do
            q "INSERT INTO clickdoom.$table \
               SELECT * REPLACE (toUInt32(200000 + n) AS tic) \
               FROM (SELECT * FROM clickdoom.$table WHERE tic = 20) \
               ARRAY JOIN range($pad) AS n" >/dev/null
        done
    fi
    state=$(q "SELECT count() FROM clickdoom.native_state FORMAT TSV")
    stage=$(q "SELECT count() FROM clickdoom.native_stage FORMAT TSV")
    sleep 3
    since=$(date -u +'%Y-%m-%d %H:%M:%S')
    for _ in $(seq 1 60); do
        q "SELECT /*LOOKUP*/ toUInt8(joinGetOrNull('clickdoom.native_state', \
           'unresolved', toUInt32(20)) IS NOT NULL)" >/dev/null
    done
    for _ in $(seq 1 3); do curl -s "$ch" --data-binary @"$statement" >/dev/null; done
    sleep 2
    q "SYSTEM FLUSH LOGS" >/dev/null
    lookup=$(spent "$since" '/*LOOKUP*/')
    whole=$(spent "$since" 'INSERT INTO clickdoom.native_stage')
    printf '%s\t%s\t%s\t%s\t%s\n' "$pad" "$state" "$stage" "$lookup" "$whole"
done
