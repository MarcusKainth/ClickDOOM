#!/usr/bin/env bash
# A1 pass 1: WHICH constructs are JIT-compilable, and does a non-compilable
# node poison its enclosing subtree?
#
# Ground truth is system.query_log's ProfileEvents, not wall clock.
# `min_count_to_compile_expression = 0` makes compilation happen on the
# FIRST execution, so one run per variant is decisive.
#
# Emits TSV to stdout:
#   variant<TAB>ast_nodes<TAB>islands<TAB>compile_us<TAB>compile_bytes<TAB>ms<TAB>sink
#
# `islands` = ProfileEvents['CompileFunction'] = distinct LLVM functions
# built for this query. Read against `chain_only` (exactly 1).
#
# Determinism: fixture seeded from `number`; no now(), no rand() (SPEC §8.1).  # purity-ok: documents the absence of now()/rand(), doesn't call either
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
K="${A1_K:-200000}"
LINKS="${A1_LINKS:-24}"
MINCOUNT="${A1_MINCOUNT:-0}"
JIT="${A1_JIT:-1}"
TAG="${A1_TAG:-a1}"
VARIANTS="${A1_VARIANTS:-$(python3 gen.py --list)}"

# The parser-limit flags must be CLIENT-SIDE: a SETTINGS clause at the end of
# the statement is itself parsed only after the body, so it cannot raise a
# limit the body already tripped. Deeply-nested fragmentation variants trip
# max_parser_backtracks; production fold.py does not.
ch() { docker exec -i "$CONTAINER" clickhouse-client --max_ast_depth 200000 --max_parser_depth 20000 --max_parser_backtracks 200000000 "$@"; }

if [ "${A1_SETUP:-1}" = "1" ]; then
  echo "# setting up a1_jit_bench fixture..." >&2
  ch --multiquery < setup.sql
fi

printf 'variant\tast_nodes\tislands\tcompile_us\tcompile_bytes\tms\tsink\n'
for V in $VARIANTS; do
  python3 gen.py "$V" "$K" "$JIT" "$MINCOUNT" "$LINKS" > /tmp/a1_$V.sql
  NODES=$({ echo "EXPLAIN AST"; cat /tmp/a1_$V.sql; } | ch --multiquery | wc -l | tr -d ' ')
  QID="${TAG}_${V}_$$"
  SINK=$(ch --query_id "$QID" --multiquery < /tmp/a1_$V.sql | tr '\t' '/')
  ch --query "SYSTEM FLUSH LOGS" >/dev/null
  ROW=$(ch --query "SELECT ProfileEvents['CompileFunction'], ProfileEvents['CompileExpressionsMicroseconds'], ProfileEvents['CompileExpressionsBytes'], query_duration_ms FROM system.query_log WHERE query_id='$QID' AND type='QueryFinish' LIMIT 1")
  printf '%s\t%s\t%s\t%s\n' "$V" "$NODES" "$ROW" "$SINK"
done

echo "# done. drop the fixture with:" >&2
echo "#   docker exec -i $CONTAINER clickhouse-client --query 'DROP DATABASE a1_jit_bench'" >&2
