#!/usr/bin/env bash
# Measures one resident stage1/stage2 analysis alone, four at once, and four
# at once beside one run_statement batch tic. Prints system.query_log rows.
set -uo pipefail

CH="http://localhost:8123/"
q() { curl -fsS -u "default:${CLICKHOUSE_PASSWORD}" --data-binary "$1" "$CH"; }
BIN=target/release/clickdoom
FIXTURE=$(ls refemu/probe/fixtures/*.tsv)

echo "== runner"
nproc
grep -m1 'model name' /proc/cpuinfo
free -g | sed -n 1,2p
q "SELECT version()"

load() {
  q "CREATE DATABASE IF NOT EXISTS $1"
  "$BIN" native load --fresh --database "$1" </dev/null >"load-$1.log" 2>&1
  echo "load $1 exit=$?"
}
diffrun() {
  local start end code
  start=$(date +%s)
  "$BIN" native diff 2 --probe "$FIXTURE" --database "$1" </dev/null >"diff-$1.log" 2>&1
  code=$?
  end=$(date +%s)
  echo "diff $1 exit=$code wall=$((end - start))s"
  tail -n 2 "diff-$1.log"
}

for d in pa pb1 pb2 pb3 pb4 pc1 pc2 pc3 pc4; do load "$d"; done

echo "== (a) one session alone"
diffrun pa

echo "== (b) four sessions at once"
for d in pb1 pb2 pb3 pb4; do diffrun "$d" & done
wait

echo "== (c) four sessions at once beside one run_statement batch tic"
for d in pc1 pc2 pc3 pc4; do diffrun "$d" & done
(
  start=$(date +%s)
  "$BATCH_BIN" --test-threads 1 >batch.log 2>&1
  code=$?
  end=$(date +%s)
  echo "batch exit=$code wall=$((end - start))s"
  tail -n 3 batch.log
) &
wait

q "SYSTEM FLUSH LOGS"
echo "== query_log, INSERTs whose analysis took over a second"
q "
SELECT
    replaceRegexpOne(extract(query, 'INSERT INTO\\s+([A-Za-z0-9_]+)\\.'), '^clickdoom_native_test_[0-9]+_', '') AS db,
    multiIf(query_id LIKE '%-sim2-%', 'resident stage2',
            query_id LIKE '%-sim-%', 'resident stage1',
            match(query, 'INSERT INTO\\s+\\S+\\.native_stage\\s'), 'batch stage1',
            'batch stage2') AS kind,
    round(ProfileEvents['QueryAnalysisMicroseconds'] / 1e6, 1) AS analysis_s,
    round(ProfileEvents['UserTimeMicroseconds'] / 1e6, 1) AS user_s,
    round(ProfileEvents['SystemTimeMicroseconds'] / 1e6, 1) AS system_s,
    round(query_duration_ms / 1e3, 1) AS duration_s,
    formatReadableSize(memory_usage) AS peak_memory,
    type,
    event_time
FROM system.query_log
WHERE type != 'QueryStart'
  AND query_kind = 'Insert'
  AND ProfileEvents['QueryAnalysisMicroseconds'] > 1000000
ORDER BY event_time, db, kind
FORMAT PrettyCompactMonoBlock"
