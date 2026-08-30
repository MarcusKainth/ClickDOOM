#!/usr/bin/env bash
# #23 throughput evidence: fold.py (halt semantics, write-log versioning,
# address/alignment/self-modify checks) vs. the baseline fold benchmark
# (fold_predecoded.py, ADR-0002), same synthetic instruction stream.
# ADR-0004 names this file explicitly: "executor/bench/halt_overhead/ stays
# in the tree as the reproducible before/after, the same role
# executor/bench/phase0/ plays for ADR-0002" -- kept and fixed for #25
# rather than deleted, even though its own e2e loop needed the same
# batch_commit-target rewrite batch_overhead's already got.
#
# Runs against a PRIVATE database, created and dropped by this script --
# NOT the shared `clickdoom_executor` this file used before #25. That
# earlier version got silently corrupted mid-run once already (something
# else issued 101 TRUNCATE/CREATE/DROP cycles against the same shared
# tables while this script's loop was running -- see
# executor/bench/batch_overhead/run.sh's history for the full incident,
# which is where the isolated-database pattern this file now copies was
# first adopted). #25 gave this script a second, independent reason to
# switch: its e2e loop calls `fold.py --e2e`, which now targets SPEC §5's
# `batch_commit` directly rather than a `batch_out`/`state` stand-in --
# schema_fixture.sql no longer creates either of those tables (#25 removed
# them, since batch_commit is real now), so this script broke outright
# until rewritten. Every table's DDL -- `ram`/`decoded`/`batch_commit`/
# `cpu_state`/`console_out`/`input_queue` -- is generated from the REAL
# sqlcpu/schema.sql (renamed via sed), not a hand-copied approximation, so
# it can't drift from what sqlcpu maintains; setup.sql only adds the
# synthetic instruction mix (same pattern batch_overhead's already uses).
#
# Emits TSV: variant<TAB>mode<TAB>K<TAB>seconds<TAB>instr_per_sec, comparable
# line for line against executor/bench/phase0/RESULTS.md's "fold in
# isolation" table.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
KS="${CLICKDOOM_BENCH_KS:-10000 50000 200000}"
REPEATS="${CLICKDOOM_BENCH_REPEATS:-2}"
HWM="${CLICKDOOM_BENCH_HWM:-20000}"
BENCH_DB="${CLICKDOOM_BENCH_DB:-clickdoom_exec_halt_bench}"

ch() { docker exec -i "$CONTAINER" clickhouse-client "$@"; }

cleanup() { ch --query "DROP DATABASE IF EXISTS $BENCH_DB"; }
trap cleanup EXIT

echo "# setting up isolated database $BENCH_DB (real sqlcpu/schema.sql, renamed)..." >&2
ch --query "DROP DATABASE IF EXISTS $BENCH_DB"
sed -E "s/clickdoom([.;])/${BENCH_DB}\\1/g" ../../../sqlcpu/schema.sql | ch --multiquery
sed "s/{{DB}}/$BENCH_DB/g" setup.sql | ch --multiquery

time_query() {
  local start end
  start=$(date +%s.%N)
  ch --multiquery > /dev/null
  end=$(date +%s.%N)
  python3 -c "print(f'{$end - $start:.3f}')"
}

printf 'variant\tmode\tK\tseconds\tinstr_per_sec\n'

emit_row() { printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$5" \
             "$(python3 -c "import sys; print(int(int(sys.argv[1]) / float(sys.argv[2])))" "$4" "$5")"; }

for K in $KS; do
  for _ in $(seq 1 "$REPEATS"); do
    emit_row halt_semantics fold "$K" "$K" \
      "$(python3 ../../fold.py "$K" --hwm "$HWM" --db "$BENCH_DB" | time_query)"
  done
done

# --- end to end: state reload + fold + write-log/cpu_state flush, via #25's
# real executor/commit.py (not a hand-copied ad-hoc flush -- that stand-in
# is gone now that batch_commit is real). This is the number ADR-0001's
# original ">=10,000 instr/sec sustained end-to-end" criterion meant --
# ADR-0004 retired that threshold as a merge gate (it predates SPEC §1's
# halt semantics being priced in), so this no longer gates anything, but
# it's still the e2e figure, distinct from the fold-in-isolation one above,
# and this bench is ADR-0004's named source for reproducing it.
python3 ../../commit.py ram --db "$BENCH_DB" > /tmp/clickdoom_halt_ram_flush.sql
python3 ../../commit.py cpu_state --db "$BENCH_DB" > /tmp/clickdoom_halt_cpu_state_flush.sql

for K in $KS; do
  BATCHES=$(( 600000 / K ))
  if [ "$BATCHES" -lt 3 ]; then BATCHES=3; fi
  ch --query "TRUNCATE TABLE $BENCH_DB.batch_commit"
  ch --query "TRUNCATE TABLE $BENCH_DB.cpu_state"
  # Seed batch_commit's batch_id=0 row via the real bootstrap.py, not a
  # hand-copied INSERT -- SPEC §1's reset state, same script the driver
  # (#28) will run once before its first batch.
  python3 ../../bootstrap.py --database "$BENCH_DB" \
    --client "docker exec -i $CONTAINER clickhouse-client"
  python3 ../../fold.py "$K" --hwm "$HWM" --e2e --db "$BENCH_DB" > /tmp/clickdoom_halt_batch.sql
  START=$(date +%s.%N)
  for _ in $(seq 1 "$BATCHES"); do
    ch --multiquery < /tmp/clickdoom_halt_batch.sql
    # RAM_BASE_WORD + wl_addr conversion and the absolute-icount version are
    # both commit.py's job now (#101) -- no `icount_before +` patch-up here,
    # unlike this script's pre-#25 version.
    ch --multiquery < /tmp/clickdoom_halt_ram_flush.sql
    ch --multiquery < /tmp/clickdoom_halt_cpu_state_flush.sql
  done
  END=$(date +%s.%N)
  emit_row halt_semantics e2e "$K" "$(( K * BATCHES ))" "$(python3 -c "print(f'{$END-$START:.3f}')")"
done
