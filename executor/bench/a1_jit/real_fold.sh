#!/usr/bin/env bash
# A1 pass 3: the SAME probe applied to the REAL production step expression.
#
# `executor/fold.py` is read-only here -- this generates its SQL unmodified,
# pointed at this experiment's own database (`--db a1_jit_bench`, the
# override fold.py already provides for exactly this), and reads the JIT
# counters off system.query_log.
#
# Two K values are run so the per-instruction MARGINAL cost can be separated
# from the large fixed per-batch cost (groupArray of the 524k-row decode and
# RAM captures + analysis of a ~104,000-node AST); the fixed cost is the
# same for both and cancels in the difference.
#
# IMPORTANT: run this against a container whose compiled-expression cache is
# COLD for this expression. That cache is server-global and keyed by each
# island's DAG, so on a container where any other agent has already run the
# real fold, CompileFunction reads 0 and the query looks as if nothing
# compiled. That is exactly the trap the prior "CompileFunction = 3" reading
# fell into. `docker run --name a1-jit-ch clickhouse/clickhouse-server:26.3`
# plus setup.sql gives a clean one.
#
# Emits TSV: k<TAB>jit<TAB>run<TAB>ms<TAB>islands<TAB>compile_us<TAB>compile_bytes
set -euo pipefail
cd "$(dirname "$0")"
REPO="$(cd ../../.. && pwd)"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-a1-jit-ch}"
KS="${A1_KS:-100 20100}"
JITS="${A1_JITS:-0 1}"
REPEATS="${A1_REPEATS:-3}"
MINCOUNT="${A1_MINCOUNT:-0}"
TAG="${A1_TAG:-rf$$}"

ch() { docker exec -i "$CONTAINER" clickhouse-client --max_ast_depth 200000 --max_parser_depth 20000 --max_parser_backtracks 200000000 "$@"; }

for K in $KS; do
  python3 "$REPO/executor/fold.py" "$K" --db a1_jit_bench > "/tmp/a1rf_$K.sql"
done

printf 'k\tjit\trun\tms\tislands\tcompile_us\tcompile_bytes\n'
for R in $(seq 1 "$REPEATS"); do
  for K in $KS; do
    for J in $JITS; do
      QID="${TAG}_${K}_${J}_${R}"
      ch --query_id "$QID" --compile_expressions "$J" \
         --min_count_to_compile_expression "$MINCOUNT" \
         --multiquery < "/tmp/a1rf_$K.sql" > /dev/null
      ch --query "SYSTEM FLUSH LOGS" > /dev/null
      ROW=$(ch --query "SELECT query_duration_ms, ProfileEvents['CompileFunction'], ProfileEvents['CompileExpressionsMicroseconds'], ProfileEvents['CompileExpressionsBytes'] FROM system.query_log WHERE query_id='$QID' AND type='QueryFinish' LIMIT 1")
      printf '%s\t%s\t%s\t%s\n' "$K" "$J" "$R" "$ROW"
    done
  done
done
