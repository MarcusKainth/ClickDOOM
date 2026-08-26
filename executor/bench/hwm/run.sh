#!/usr/bin/env bash
# Write-log high-water-mark curve (see RESULTS.md). Reproduces the numbers
# that set config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT.
set -euo pipefail
cd "$(dirname "$0")"

CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"
KS="${CLICKDOOM_HWM_KS:-2500 5000 10000 20000 40000 80000 160000}"

ch() { docker exec -i "$CONTAINER" clickhouse-client --multiquery; }

echo "# setting up worst-case all-store fixture..." >&2
python3 gen.py schema | ch > /dev/null

printf 'K\tseconds\tus_per_step\n'
for K in $KS; do
  START=$(date +%s.%N)
  python3 gen.py "$K" | ch > /dev/null
  END=$(date +%s.%N)
  python3 -c "
sec = $END - $START
print(f'{$K}\t{sec:.3f}\t{sec*1e6/$K:.2f}')
"
done
