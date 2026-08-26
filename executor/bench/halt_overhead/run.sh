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

printf 'variant\tK\tseconds\tinstr_per_sec\n'
for K in $KS; do
  for _ in $(seq 1 "$REPEATS"); do
    emit halt_semantics "$K" "$K" "$(python3 ../../fold.py "$K" --hwm "$HWM" | time_query)"
  done
done
