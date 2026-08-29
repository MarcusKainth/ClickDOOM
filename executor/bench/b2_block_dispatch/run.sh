#!/usr/bin/env bash
# B2: cost of an unselected branch inside arrayFold, on a fresh container of the repo pin.
# Usage: run.sh [--k 20000] [--links 100] [--repeats 3] [--out DIR]
# Take the machine lock (kind: timing) first.
set -euo pipefail
cd "$(dirname "$0")/../../.."
HERE="executor/bench/b2_block_dispatch"
K=20000; LINKS=100; REPEATS=3
OUT="${TMPDIR:-/tmp}/clickdoom-b2/$(date -u +%Y%m%dT%H%M%SZ)"
PORT=9040
CLIENT="${CLICKDOOM_NATIVE_CLICKHOUSE:-$HOME/.clickhouse/26.3.25.2/clickhouse} client"
while [ $# -gt 0 ]; do
  case "$1" in
    --k) K="$2"; shift 2 ;;
    --links) LINKS="$2"; shift 2 ;;
    --repeats) REPEATS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
mkdir -p "$OUT"
IMAGE=$(grep -oE 'clickhouse/clickhouse-server:[^ ]+' docker-compose.yml | head -1)
SALT=$(date +%s)   # fresh literals: the compiled-expression cache is keyed on the DAG
# shellcheck disable=SC2206
CH=($CLIENT --host 127.0.0.1 --port "$PORT" --password clickdoom)

docker rm -f b2-ch >/dev/null 2>&1 || true
docker run -d --name b2-ch --ulimit nofile=262144 -e CLICKHOUSE_PASSWORD=clickdoom -p "127.0.0.1:$PORT:9000" "$IMAGE" >/dev/null
trap 'docker rm -f b2-ch >/dev/null 2>&1 || true' EXIT
for _ in $(seq 1 100); do "${CH[@]}" --query "SELECT 1" >/dev/null 2>&1 </dev/null && break; sleep 0.3; done
echo "# image=$IMAGE version=$("${CH[@]}" --query 'SELECT version()' </dev/null) K=$K links=$LINKS repeats=$REPEATS salt=$SALT" | tee "$OUT/run.log" >&2

run_timed() {  # name sql settings -> prints seconds (best of REPEATS) and all runs
  local name=$1 sql=$2 settings=$3 best="" t
  for _ in $(seq 1 "$REPEATS"); do
    t=$("${CH[@]}" --time --query "$sql SETTINGS max_threads=1, $settings" </dev/null 2>&1 >/dev/null | tail -1)
    printf '%s\t%s\t%s\n' "$name" "$settings" "$t" >> "$OUT/runs.tsv"
    if [ -z "$best" ] || [ "$(python3 -c "print(1 if $t < $best else 0)")" = 1 ]; then best=$t; fi
  done
  printf '%s\t%s\t%s\n' "$name" "$settings" "$best"
}

printf 'variant\tsettings\tbest_seconds\n' | tee "$OUT/results.tsv"
while IFS=$'\t' read -r name sql; do
  case "$name" in
    fault_*)
      out=$("${CH[@]}" --query "$sql SETTINGS max_threads=1" </dev/null 2>&1 | head -1 | cut -c1-120)
      case "$out" in
        *ILLEGAL_DIVISION*|*"Division by zero"*) r="FAULT (branch evaluated)" ;;
        *Exception*|*Code:*) r="ERROR: $out" ;;
        *) r="ok, value=$out (branch not evaluated)" ;;
      esac
      printf '%s\t-\t%s\n' "$name" "$r" | tee -a "$OUT/results.tsv" ;;
    if_guarded|multiif_guarded)
      run_timed "$name" "$sql" "compile_expressions=0" | tee -a "$OUT/results.tsv"
      run_timed "$name" "$sql" "compile_expressions=0, short_circuit_function_evaluation='force_enable'" | tee -a "$OUT/results.tsv" ;;
    *)
      run_timed "$name" "$sql" "compile_expressions=0" | tee -a "$OUT/results.tsv" ;;
  esac
done < <(python3 "$HERE/gen.py" --k "$K" --links "$LINKS" --salt "$SALT")
echo "# results: $OUT/results.tsv" >&2
