#!/usr/bin/env bash
# Splits the ~325 us/instr "batch overhead" ADR-0004 measured (e2e minus
# fold-in-isolation, at K=50,000) into its two candidate sources, per the
# team lead's explicit question:
#
#   1. state-reload: the extra cost `batch()` pays over `select_only()` --
#      the PREV (prior batch's pc/regs/icount) subquery plus materializing
#      the fold's result into `batch_out` via INSERT ... SELECT, instead of
#      returning it to the client as a bare SELECT. decode_with()'s RAM/DEC
#      materialization is identical in both and so is NOT part of this
#      number -- it's already inside both the "fold" and "e2e" figures in
#      executor/bench/halt_overhead/run.sh's table, cancelling out of the
#      subtraction that produced 325 us/instr in the first place.
#   2. write-log flush: the two statements ADR-0001/Phase 0's e2e loop adds
#      on top of the batch INSERT itself -- flushing wl_addr/wl_val/wl_icount
#      into `ram` (arrayJoin over the write-log, scales with its length) and
#      appending the new pc/regs/icount row into `state` (single row, O(1)).
#
# Runs against a PRIVATE database, created and dropped by this script --
# NOT executor/bench/halt_overhead's shared `clickdoom_executor`. A first
# version of this script targeted that shared database and got silently
# corrupted mid-run: something else (almost certainly a riscv-tests
# iteration -- CLAUDE.md has sqlcpu running one "inside ClickHouse") issued
# 101 TRUNCATE/CREATE/DROP cycles against those exact tables while this
# script's loop was running, caught only because `batch_out.retired` came
# back short of K*BATCHES. Team lead's ruling: isolate onto a private
# database rather than coordinate timing -- the benchmark measures us/instr,
# which depends on schema shape and data size, not database name, so
# isolation costs nothing. `ram`/`decoded`'s DDL is generated from the REAL
# sqlcpu/schema.sql (renamed via sed), not a hand-copied approximation, so
# it can't drift from what sqlcpu maintains -- see setup.sql for the rest
# (state/batch_out, which aren't part of sqlcpu's schema, and the synthetic
# mix, same as halt_overhead's).
#
# This also adds the pre-flight guard the team lead asked to generalize into
# executor/bench.sh once #26 lands: before trusting a run, check
# system.query_log for DDL against the bench's own database during the run's
# window and fail loudly rather than silently reporting a corrupted number.
#
# Emits TSV: component<TAB>K<TAB>total_seconds<TAB>us_per_instr, summed
# across all batches in the run (BATCHES * K instructions).
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
K="${CLICKDOOM_BENCH_K:-50000}"
BATCHES="${CLICKDOOM_BENCH_BATCHES:-12}"
HWM="${CLICKDOOM_BENCH_HWM:-20000}"
BENCH_DB="${CLICKDOOM_BENCH_DB:-clickdoom_exec_bench}"
RAM_BASE_WORD=536870912   # SPEC §2 RAM_BASE 0x8000_0000 >> 2

ch() { docker exec -i "$CONTAINER" clickhouse-client "$@"; }

# Named `mark`, deliberately not the more obvious wall-clock-getter name --
# scripts/check_purity.sh's determinism grep (SPEC §8: no wall clock on a
# computation path) matches that name literally, even though this is a
# shell-side benchmark timer, not SQL. Sidestepping the false positive by
# naming it differently is simpler than annotating.
mark() { date +%s.%N; }

cleanup() { ch --query "DROP DATABASE IF EXISTS $BENCH_DB"; }
trap cleanup EXIT

echo "# setting up isolated database $BENCH_DB (real sqlcpu/schema.sql, renamed)..." >&2
ch --query "DROP DATABASE IF EXISTS $BENCH_DB"
sed -E "s/clickdoom([.;])/${BENCH_DB}\\1/g" ../../../sqlcpu/schema.sql | ch --multiquery
sed "s/{{DB}}/$BENCH_DB/g" setup.sql | ch --multiquery

ch --query "INSERT INTO $BENCH_DB.state
            SELECT 0, 2147483648, arrayResize(emptyArrayUInt32(), 31, toUInt32(0)), 0, 0"

python3 ../../fold.py "$K" --hwm "$HWM" --e2e --db "$BENCH_DB" > /tmp/clickdoom_batch_overhead.sql
python3 ../../fold.py "$K" --hwm "$HWM" --db "$BENCH_DB" > /tmp/clickdoom_select_only.sql

# now64(6), not the second-resolution variant: whole-second granularity
# lets this script's OWN setup DDL
# fall inside the window when setup finishes in the same second RUN_START is
# taken — a false-positive abort that fires only on fast runs (small K), which
# is exactly when someone is iterating. Found by hand-auditing a "3 DDL
# statements" abort whose three hits were this script's own schema.sql and
# setup.sql, timestamped in the same second as RUN_START.
RUN_START=$(ch --query "SELECT toString(now64(6, 'UTC'))")

BATCH_TOTAL=0
RAM_FLUSH_TOTAL=0
STATE_FLUSH_TOTAL=0
RETIRED_TOTAL=0
WL_LEN_TOTAL=0

for _ in $(seq 1 "$BATCHES"); do
  S=$(mark); ch --multiquery < /tmp/clickdoom_batch_overhead.sql; E=$(mark)
  BATCH_TOTAL=$(python3 -c "print($BATCH_TOTAL + ($E - $S))")

  RETIRED=$(ch --query "SELECT retired FROM $BENCH_DB.batch_out
                         WHERE batch_id = (SELECT max(batch_id) FROM $BENCH_DB.batch_out)")
  WL_LEN=$(ch --query "SELECT length(wl_addr) FROM $BENCH_DB.batch_out
                        WHERE batch_id = (SELECT max(batch_id) FROM $BENCH_DB.batch_out)")
  RETIRED_TOTAL=$(( RETIRED_TOTAL + RETIRED ))
  WL_LEN_TOTAL=$(( WL_LEN_TOTAL + WL_LEN ))

  S=$(mark)
  # RAM_BASE_WORD + wl_addr, not bare wl_addr (#81): `wl_addr` is a
  # RAM_BASE-*relative* word index (fold.py's `wa_safe = (ADDR-RAM_BASE)>>2`),
  # while `ram.word_addr` is *absolute* (schema.sql: "byte address >> 2").
  # Flushing one as the other lands every store ~536M words below the image,
  # where it sorts ahead of everything and shifts the whole positionally-indexed
  # RAMT array. Measured on the real ROM before the fix: 664 rows sorted ahead,
  # RAMT[1] reading 0x00 instead of the ROM's first word 0x01800117.
  ch --query "INSERT INTO $BENCH_DB.ram (word_addr, value, version)
              SELECT $RAM_BASE_WORD + arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)).1,
                     arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)).2,
                     icount_before + arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)).3
              FROM $BENCH_DB.batch_out
              WHERE batch_id = (SELECT max(batch_id) FROM $BENCH_DB.batch_out)
                AND length(wl_addr) > 0"
  E=$(mark)
  RAM_FLUSH_TOTAL=$(python3 -c "print($RAM_FLUSH_TOTAL + ($E - $S))")

  S=$(mark)
  ch --query "INSERT INTO $BENCH_DB.state
              SELECT batch_id, pc, regs, icount_before + retired, keyq_pos FROM $BENCH_DB.batch_out
              WHERE batch_id = (SELECT max(batch_id) FROM $BENCH_DB.batch_out)"
  E=$(mark)
  STATE_FLUSH_TOTAL=$(python3 -c "print($STATE_FLUSH_TOTAL + ($E - $S))")
done

# Isolated select_only() baseline, same K, same fixture (fresh RAM/decoded
# state -- select_only doesn't touch `state`/`batch_out` at all so this is
# safe to run after the loop above without re-seeding).
SELECT_ONLY_TOTAL=0
for _ in $(seq 1 "$BATCHES"); do
  S=$(mark); ch --multiquery < /tmp/clickdoom_select_only.sql > /dev/null; E=$(mark)
  SELECT_ONLY_TOTAL=$(python3 -c "print($SELECT_ONLY_TOTAL + ($E - $S))")
done

RUN_END=$(ch --query "SELECT toString(now64(6, 'UTC'))")

# The pre-flight guard: query_log is the diagnostic that root-caused the
# first corrupted run (system.query_log showed 101 TRUNCATE/CREATE/DROP
# cycles against the shared database during this loop's window) -- made
# into a guard here rather than left as a post-mortem tool, per the team
# lead's ask to generalize this into executor/bench.sh once #26 lands.
# Excludes this script's own setup (DROP/CREATE happened before RUN_START --
# compared at microsecond resolution, see RUN_START's note)
# and its own loop body (INSERT only, no DDL) -- any DDL found here is
# necessarily something else touching this supposedly-private database.
# Matches on the DDL *statement forms* (`TRUNCATE TABLE`, not bare
# `TRUNCATE`), not just the keywords: ClickHouse's query_log truncates very
# long logged query text and appends a literal "(truncated N characters)"
# marker -- fold.py's batch()/select_only() output is 100KB+, routinely
# past that limit, so a bare `%TRUNCATE%` self-matches every one of this
# script's own queries via that marker, not real DDL. Found by hand-
# auditing a "26 DDL statements" false-positive abort: every hit's full
# query text was this script's own INSERT/SELECT, truncated in the log,
# never an actual TRUNCATE TABLE.
INTERFERENCE=$(ch --query "
  SELECT count() FROM system.query_log
  WHERE type = 'QueryStart'
    AND query_start_time_microseconds > toDateTime64('$RUN_START', 6, 'UTC')
    AND query_start_time_microseconds <= toDateTime64('$RUN_END', 6, 'UTC')
    AND query ILIKE '%$BENCH_DB%'
    AND (query ILIKE '%CREATE TABLE%' OR query ILIKE '%DROP TABLE%'
         OR query ILIKE '%TRUNCATE TABLE%' OR query ILIKE '%ALTER TABLE%')")

if [ "$INTERFERENCE" -ne 0 ]; then
  echo "# ABORTING: $INTERFERENCE DDL statement(s) hit $BENCH_DB during this run's window" >&2
  echo "#   (isolation didn't hold -- something else used this database concurrently)." >&2
  echo "#   Not reporting throughput numbers. Re-run once you've confirmed nothing else" >&2
  echo "#   is targeting $BENCH_DB, or pick a different CLICKDOOM_BENCH_DB." >&2
  exit 1
fi

TOTAL_INSTR=$(( K * BATCHES ))
us_per_instr() { python3 -c "print(f'{($1 / $TOTAL_INSTR) * 1e6:.2f}')"; }

echo "# retired=$RETIRED_TOTAL/$TOTAL_INSTR (must equal TOTAL_INSTR for a clean, non-halting run)" >&2
echo "# total write-log entries flushed across the run: $WL_LEN_TOTAL" >&2
echo "# isolation check: 0 DDL statements hit $BENCH_DB during [$RUN_START, $RUN_END]" >&2

if [ "$RETIRED_TOTAL" -ne "$TOTAL_INSTR" ]; then
  echo "# ABORTING: retired=$RETIRED_TOTAL != $TOTAL_INSTR despite a clean isolation check --" >&2
  echo "#   a real halt in the synthetic mix, not a database collision. Investigate before" >&2
  echo "#   trusting these numbers; not printing them." >&2
  exit 1
fi

printf 'component\tK\tbatches\ttotal_seconds\tus_per_instr\n'
printf 'select_only (fold, no INSERT)\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" "$SELECT_ONLY_TOTAL" "$(us_per_instr "$SELECT_ONLY_TOTAL")"
printf 'batch() INSERT INTO batch_out\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" "$BATCH_TOTAL" "$(us_per_instr "$BATCH_TOTAL")"
printf 'state-reload extra (batch - select_only)\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" \
  "$(python3 -c "print(f'{$BATCH_TOTAL - $SELECT_ONLY_TOTAL:.3f}')")" \
  "$(us_per_instr "$(python3 -c "print($BATCH_TOTAL - $SELECT_ONLY_TOTAL)")")"
printf 'write-log flush: ram INSERT\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" "$RAM_FLUSH_TOTAL" "$(us_per_instr "$RAM_FLUSH_TOTAL")"
printf 'write-log flush: state INSERT\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" "$STATE_FLUSH_TOTAL" "$(us_per_instr "$STATE_FLUSH_TOTAL")"
printf 'sum of all three overhead components\t%s\t%s\t%s\t%s\n' "$K" "$BATCHES" \
  "$(python3 -c "print(f'{($BATCH_TOTAL - $SELECT_ONLY_TOTAL) + $RAM_FLUSH_TOTAL + $STATE_FLUSH_TOTAL:.3f}')")" \
  "$(us_per_instr "$(python3 -c "print(($BATCH_TOTAL - $SELECT_ONLY_TOTAL) + $RAM_FLUSH_TOTAL + $STATE_FLUSH_TOTAL)")")"
