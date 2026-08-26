#!/usr/bin/env bash
# Experiment E1: common-subexpression elimination inside an arrayFold lambda.
# See README.md for the question and the recorded results.
#
# Emits TSV to stdout:
#   variant<TAB>jit<TAB>run<TAB>K<TAB>seconds<TAB>ast_nodes<TAB>sql_bytes<TAB>sink
#
# Client-side wall clock only. That is measurement, not computation
# (PURITY.md); no number here feeds any emulated-CPU state. The fixture is
# seeded from `number` -- no now(), no rand() (SPEC §8.1).  # purity-ok: documents the absence of now()/rand(), doesn't call either
#
# Repeats are INTERLEAVED (repeat outer, variant inner) so a slow patch on
# the shared container spreads across variants instead of landing entirely
# on one of them.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
K="${E1_K:-50000}"
REPEATS="${E1_REPEATS:-5}"
JIT="${E1_JIT:-0}"          # compile_expressions; 0 = interpreted (default)
VARIANTS="${E1_VARIANTS:-floor n1 n2 n5 n10 n20 n40 n40_ws n40_plus0_same n40_plus0_distinct n40_bound}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ch() { docker exec -i "$CONTAINER" clickhouse-client "$@"; }

echo "# setting up e1_cse_bench fixture (524288 pre-decoded words)..." >&2
ch --multiquery < setup.sql

# Generate every query up front, and measure its parsed-AST size once.
# EXPLAIN AST reports the pre-analysis tree -- the same thing the "~90,000
# nodes" figure for fold.py's step expression counts, so it is the right
# x-axis for a node-budget argument.
for V in $VARIANTS; do
  python3 gen.py "$V" "$K" "$JIT" > "$TMP/$V.sql"
  { echo "EXPLAIN AST"; cat "$TMP/$V.sql"; } > "$TMP/$V.ast.sql"
  ch --multiquery < "$TMP/$V.ast.sql" | wc -l | tr -d ' ' > "$TMP/$V.nodes"
done

printf 'variant\tjit\trun\tK\tseconds\tast_nodes\tsql_bytes\tsink\n'
for R in $(seq 1 "$REPEATS"); do
  for V in $VARIANTS; do
    S=$(python3 -c 'import time; print(time.time())')
    SINK=$(ch --multiquery < "$TMP/$V.sql" | tr '\t' '/')
    E=$(python3 -c 'import time; print(time.time())')
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$V" "$JIT" "$R" "$K" "$(python3 -c "print(f'{$E - $S:.3f}')")" \
      "$(cat "$TMP/$V.nodes")" "$(wc -c < "$TMP/$V.sql" | tr -d ' ')" "$SINK"
  done
done

echo "# done. drop the fixture with:" >&2
echo "#   docker exec -i $CONTAINER clickhouse-client --query 'DROP DATABASE e1_cse_bench'" >&2
