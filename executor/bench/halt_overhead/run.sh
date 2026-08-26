#!/usr/bin/env bash
# #23 throughput evidence: fold.py (halt semantics, write-log versioning,
# address/alignment/self-modify checks) vs. the Phase 0 baseline
# (fold_predecoded.py, ADR-0002), same synthetic instruction stream.
#
# Emits TSV: variant<TAB>K<TAB>seconds<TAB>instr_per_sec, comparable line for
# line against executor/bench/phase0/RESULTS.md's "fold in isolation" table.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
KS="${CLICKDOOM_BENCH_KS:-10000 50000 200000}"
REPEATS="${CLICKDOOM_BENCH_REPEATS:-2}"
HWM="${CLICKDOOM_BENCH_HWM:-20000}"

ch() { docker exec -i "$CONTAINER" clickhouse-client "$@"; }

time_query() {
  local start end
  start=$(date +%s.%N)
  ch --multiquery > /dev/null
  end=$(date +%s.%N)
  python3 -c "print(f'{$end - $start:.3f}')"
}

emit() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$4" \
         "$(python3 -c "import sys; print(int(int(sys.argv[1]) / float(sys.argv[2])))" "$3" "$4")"; }

echo "# setting up fixtures (24 MiB RAM, 2 MiB decoded text, Phase 0's mix)..." >&2
ch --multiquery < ../../schema_fixture.sql
ch --multiquery < setup.sql

printf 'variant\tmode\tK\tseconds\tinstr_per_sec\n'

emit_row() { printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$5" \
             "$(python3 -c "import sys; print(int(int(sys.argv[1]) / float(sys.argv[2])))" "$4" "$5")"; }

for K in $KS; do
  for _ in $(seq 1 "$REPEATS"); do
    emit_row halt_semantics fold "$K" "$K" "$(python3 ../../fold.py "$K" --hwm "$HWM" | time_query)"
  done
done

# --- end to end: state reload + fold + write-log/state flush, matching
# Phase 0's run.sh e2e loop (same ad-hoc flush -- the real atomic-commit
# design is #25, still blocked on ratification; this is a throughput
# measurement against ADR-0001's e2e threshold, not the shipped commit
# mechanism). ADR-0001's criterion is explicitly this number, not the
# fold-in-isolation one above -- ask for this, not the other, when checking
# against the >=10,000 instr/sec threshold.
for K in $KS; do
  BATCHES=$(( 600000 / K ))
  if [ "$BATCHES" -lt 3 ]; then BATCHES=3; fi
  ch --query "TRUNCATE TABLE clickdoom_executor.state"
  ch --query "TRUNCATE TABLE clickdoom_executor.batch_out"
  ch --query "INSERT INTO clickdoom_executor.state
              SELECT 0, 2147483648, arrayResize(emptyArrayUInt32(), 31, toUInt32(0)), 0"
  python3 ../../fold.py "$K" --hwm "$HWM" --e2e > /tmp/clickdoom_executor_batch.sql
  START=$(date +%s.%N)
  for _ in $(seq 1 "$BATCHES"); do
    ch --multiquery < /tmp/clickdoom_executor_batch.sql
    ch --query "INSERT INTO clickdoom_executor.ram
                SELECT arrayJoin(arrayZip(wl_addr, wl_val)).1,
                       arrayJoin(arrayZip(wl_addr, wl_val)).2,
                       icount_before + arrayJoin(wl_icount)
                FROM clickdoom_executor.batch_out
                WHERE batch_id = (SELECT max(batch_id) FROM clickdoom_executor.batch_out)
                  AND length(wl_addr) > 0"
    ch --query "INSERT INTO clickdoom_executor.state
                SELECT batch_id, pc, regs, icount_before + retired FROM clickdoom_executor.batch_out
                WHERE batch_id = (SELECT max(batch_id) FROM clickdoom_executor.batch_out)"
  done
  END=$(date +%s.%N)
  emit_row halt_semantics e2e "$K" "$(( K * BATCHES ))" "$(python3 -c "print(f'{$END-$START:.3f}')")"
done
