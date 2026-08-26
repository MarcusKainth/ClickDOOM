#!/usr/bin/env bash
# A1 pass 2: the per-run TIMING series.
#
# Two uses:
#   * A1_MINCOUNT=3 (ClickHouse's default) + A1_REPEATS>=6 shows the STEP
#     CHANGE on the 4th execution -- the signal that the JIT engaged. An
#     average would hide it, so the per-run series is what gets reported.
#   * A1_JIT=0 vs A1_JIT=1 with A1_MINCOUNT=0 prices the JIT's actual win
#     at a given level of island fragmentation.
#
# Timing comes from system.query_log's query_duration_ms (server-side,
# query-scoped), not from client wall clock, together with the compile
# counters -- wall clock alone proves nothing (that is how the icount_base
# cache-key bug hid for weeks).
#
# Emits TSV: variant<TAB>jit<TAB>min_count<TAB>run<TAB>ms<TAB>islands<TAB>compile_us
#
# Repeats are INTERLEAVED (repeat outer, variant inner) so a slow patch on
# the machine spreads across variants instead of landing on one.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-a1-jit-ch}"
K="${A1_K:-100000}"
LINKS="${A1_LINKS:-24}"
REPEATS="${A1_REPEATS:-6}"
JIT="${A1_JIT:-1}"
MINCOUNT="${A1_MINCOUNT:-0}"
TAG="${A1_TAG:-s$$}"
VARIANTS="${A1_VARIANTS:-frag_none frag_2 frag_4 frag_8 frag_12}"

ch() { docker exec -i "$CONTAINER" clickhouse-client --max_ast_depth 200000 --max_parser_depth 20000 --max_parser_backtracks 200000000 "$@"; }

for V in $VARIANTS; do
  python3 gen.py "$V" "$K" "$JIT" "$MINCOUNT" "$LINKS" > "/tmp/a1s_$V.sql"
done

printf 'variant\tjit\tmin_count\trun\tms\tislands\tcompile_us\n'
for R in $(seq 1 "$REPEATS"); do
  for V in $VARIANTS; do
    QID="${TAG}_${V}_${JIT}_${MINCOUNT}_${R}"
    ch --query_id "$QID" --multiquery < "/tmp/a1s_$V.sql" > /dev/null
    ch --query "SYSTEM FLUSH LOGS" > /dev/null
    ROW=$(ch --query "SELECT query_duration_ms, ProfileEvents['CompileFunction'], ProfileEvents['CompileExpressionsMicroseconds'] FROM system.query_log WHERE query_id='$QID' AND type='QueryFinish' LIMIT 1")
    printf '%s\t%s\t%s\t%s\t%s\n' "$V" "$JIT" "$MINCOUNT" "$R" "$ROW"
  done
done
