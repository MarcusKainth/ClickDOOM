#!/usr/bin/env bash
# Phase 0 arrayFold throughput benchmark (SPEC §9, evidence for ADR-0001/0002).
#
# Emits TSV to stdout: variant<TAB>mode<TAB>K<TAB>seconds<TAB>instr_per_sec
# so results can be pasted into an ADR or loaded straight back into ClickHouse.
#
# This harness times query wall-clock on the client side. That is measurement,
# not computation -- PURITY.md allows the driver to record timings -- and no
# benchmark number feeds any emulated-CPU state.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
KS="${CLICKDOOM_BENCH_KS:-10000 50000 200000}"
REPEATS="${CLICKDOOM_BENCH_REPEATS:-2}"

ch() { docker exec -i "$CONTAINER" clickhouse-client "$@"; }

# Time one query read from stdin; echo elapsed seconds. clickhouse-client
# --time writes elapsed seconds as the last line of its output.
time_query() { ch --time --multiquery | tail -1; }

emit() { printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" \
         "$(python3 -c "import sys; print(int(int(sys.argv[1]) / float(sys.argv[2])))" "$3" "$4")"; }

echo "# setting up fixtures (24 MiB RAM, 2 MiB pre-decoded text)..." >&2
ch --multiquery < setup.sql

printf 'variant\tmode\tK\tseconds\tinstr_per_sec\n'

# --- fold in isolation -------------------------------------------------------
for K in $KS; do
  for _ in $(seq 1 "$REPEATS"); do
    emit predecoded fold "$K" "$(python3 fold_predecoded.py "$K" | time_query)"
  done
done

# The naive variant is ~8x slower, so only sample it at the middle K.
for _ in $(seq 1 "$REPEATS"); do
  emit naive fold 10000 "$(python3 fold_naive.py 10000 splice | time_query)"
done

# --- accumulator copy behaviour (SPEC §9, second bullet) ---------------------
# If a captured constant array were copied into the accumulator per step,
# throughput would fall as the array grows. It does not; these four rows are
# the evidence.
for N in 1024 65536 1048576 6291456; do
  Q="WITH (SELECT groupArray(value) FROM (SELECT value FROM clickdoom_bench.ram FINAL WHERE word_addr < $N ORDER BY word_addr)) AS RAM
     SELECT arrayFold((acc, i) -> tuple(toUInt32(acc.1 + 4),
       arrayMap(j -> toUInt32(if(j = 5, acc.2[j] + RAM[toUInt32((acc.1 % $N) + 1)], acc.2[j])), range(1,32))),
       range(100000), tuple(toUInt32(2147483648), arrayResize(emptyArrayUInt32(), 31, toUInt32(0)))).1
     SETTINGS max_threads = 1"
  emit "ramsize_$N" fold 100000 "$(printf '%s' "$Q" | time_query)"
done

# --- end to end: state reload + fold + write-log commit ----------------------
for K in $KS; do
  BATCHES=$(( 600000 / K )); [ "$BATCHES" -lt 3 ] && BATCHES=3
  ch --query "TRUNCATE TABLE clickdoom_bench.state"
  ch --query "TRUNCATE TABLE clickdoom_bench.batch_out"
  ch --query "INSERT INTO clickdoom_bench.state
              SELECT 0, 0, arrayResize(emptyArrayUInt32(), 32, toUInt32(0)), 0"
  python3 fold_predecoded.py "$K" e2e > /tmp/clickdoom_batch.sql
  START=$(date +%s.%N)
  for _ in $(seq 1 "$BATCHES"); do
    ch --multiquery < /tmp/clickdoom_batch.sql
    ch --query "INSERT INTO clickdoom_bench.ram
                SELECT arrayJoin(arrayZip(wl_addr, wl_val)).1,
                       arrayJoin(arrayZip(wl_addr, wl_val)).2, icount
                FROM clickdoom_bench.batch_out
                WHERE batch_id = (SELECT max(batch_id) FROM clickdoom_bench.batch_out)"
    ch --query "INSERT INTO clickdoom_bench.state
                SELECT batch_id, pcidx, regs, icount FROM clickdoom_bench.batch_out
                WHERE batch_id = (SELECT max(batch_id) FROM clickdoom_bench.batch_out)"
  done
  END=$(date +%s.%N)
  emit predecoded e2e "$(( K * BATCHES ))" "$(python3 -c "print(f'{$END-$START:.3f}')")"
done
