#!/usr/bin/env bash
# B3: RAM reads inside arrayFold via dictGet (FLAT and HASHED dictionaries) against
# arrayElement on a captured constant array, on a fresh container of the repo pin.
# Usage: run.sh [--k 20000] [--repeats 3] [--out DIR]
# Take the machine lock (kind: timing) first.
set -euo pipefail
cd "$(dirname "$0")/../../.."
K=20000; REPEATS=3
OUT="${TMPDIR:-/tmp}/clickdoom-b3/$(date -u +%Y%m%dT%H%M%SZ)"
PORT=9040
WORDS=6291456   # SPEC section 2: 24 MiB of RAM
CLIENT="${CLICKDOOM_NATIVE_CLICKHOUSE:-$HOME/.clickhouse/26.3.25.2/clickhouse} client"
while [ $# -gt 0 ]; do
  case "$1" in
    --k) K="$2"; shift 2 ;;
    --repeats) REPEATS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
mkdir -p "$OUT"
IMAGE=$(grep -oE 'clickhouse/clickhouse-server:[^ ]+' docker-compose.yml | head -1)
# shellcheck disable=SC2206
CH=($CLIENT --host 127.0.0.1 --port "$PORT" --password clickdoom)
q() { "${CH[@]}" --query "$1" </dev/null; }
timed() {  # name sql -> best of REPEATS seconds
  local name=$1 sql=$2 best="" t
  for _ in $(seq 1 "$REPEATS"); do
    t=$("${CH[@]}" --time --query "$sql" </dev/null 2>&1 >/dev/null | tail -1)
    printf '%s\t%s\n' "$name" "$t" >> "$OUT/runs.tsv"
    if [ -z "$best" ] || [ "$(python3 -c "print(1 if $t < $best else 0)")" = 1 ]; then best=$t; fi
  done
  printf '%s\t%s\n' "$name" "$best" | tee -a "$OUT/results.tsv"
}

docker rm -f b3-ch >/dev/null 2>&1 || true
docker run -d --name b3-ch --ulimit nofile=262144 -e CLICKHOUSE_PASSWORD=clickdoom -p "127.0.0.1:$PORT:9000" "$IMAGE" >/dev/null
trap 'docker rm -f b3-ch >/dev/null 2>&1 || true' EXIT
for _ in $(seq 1 100); do q "SELECT 1" >/dev/null 2>&1 && break; sleep 0.3; done
echo "# image=$IMAGE version=$(q 'SELECT version()') K=$K words=$WORDS repeats=$REPEATS" | tee "$OUT/run.log" >&2

q "CREATE DATABASE b3"
q "CREATE TABLE b3.mem (word_addr UInt32, value UInt32, version UInt64) ENGINE = ReplacingMergeTree(version) ORDER BY word_addr"
q "INSERT INTO b3.mem SELECT toUInt32(number), toUInt32(cityHash64(number)), 0 FROM numbers($WORDS)"
q "CREATE DICTIONARY b3.mem_flat (word_addr UInt32, value UInt32) PRIMARY KEY word_addr
   SOURCE(CLICKHOUSE(DB 'b3' TABLE 'mem' USER 'default' PASSWORD 'clickdoom'))
   LAYOUT(FLAT(initial_array_size $WORDS max_array_size $WORDS)) LIFETIME(0)"
q "CREATE DICTIONARY b3.mem_hashed (word_addr UInt32, value UInt32) PRIMARY KEY word_addr
   SOURCE(CLICKHOUSE(DB 'b3' TABLE 'mem' USER 'default' PASSWORD 'clickdoom'))
   LAYOUT(HASHED()) LIFETIME(0)"
q "SYSTEM RELOAD DICTIONARY b3.mem_flat"; q "SYSTEM RELOAD DICTIONARY b3.mem_hashed"
echo "# dictionaries loaded: $(q "SELECT name, status, element_count, formatReadableSize(bytes_allocated) FROM system.dictionaries WHERE database='b3' FORMAT TSV" | tr '\n' ';')" | tee -a "$OUT/run.log" >&2

# one pseudo-random word per step; same address stream in every variant
WA="(toUInt32(i * 2654435761) % $WORDS)"
CAPTURE="(SELECT groupArray(tuple(value)) FROM (SELECT value, word_addr FROM b3.mem FINAL ORDER BY word_addr)) AS RAMT"
fold() { echo "SELECT arrayFold((acc, i) -> tuple(acc.1 + toUInt64($1), acc.2 + 1), range($2), tuple(toUInt64(0), toUInt64(0))).1 SETTINGS max_threads = 1, compile_expressions = 0"; }

printf 'variant\tbest_seconds\n' | tee "$OUT/results.tsv"
timed "floor_K"                 "$(fold 'i' "$K")"
timed "capture_only_K1"         "WITH $CAPTURE $(fold 'length(RAMT)' 1)"
timed "arrayElement_K"          "WITH $CAPTURE $(fold "RAMT[$WA + 1].1" "$K")"
timed "dictGet_flat_K"          "$(fold "dictGet('b3.mem_flat', 'value', toUInt64($WA))" "$K")"
timed "dictGet_hashed_K"        "$(fold "dictGet('b3.mem_hashed', 'value', toUInt64($WA))" "$K")"
# four reads per step, as a batch step reads decode fields, RAM and the write-log
R4() { echo "$1(0) + $1(1) + $1(2) + $1(3)"; }
ae() { echo "RAMT[(toUInt32((i + $1) * 2654435761) % $WORDS) + 1].1"; }
dg() { echo "dictGet('b3.mem_flat', 'value', toUInt64(toUInt32((i + $1) * 2654435761) % $WORDS))"; }
timed "arrayElement_x4_K"       "WITH $CAPTURE $(fold "$(ae 0) + $(ae 1) + $(ae 2) + $(ae 3)" "$K")"
timed "dictGet_flat_x4_K"       "$(fold "$(dg 0) + $(dg 1) + $(dg 2) + $(dg 3)" "$K")"
# the flush scenario: 20,000 changed words, then a full reload of the FLAT dictionary
q "INSERT INTO b3.mem SELECT toUInt32(number * 313 % $WORDS), toUInt32(number), 1 FROM numbers(20000)"
timed "reload_flat_after_20k_stores" "SYSTEM RELOAD DICTIONARY b3.mem_flat"
timed "capture_after_20k_stores_K1"  "WITH $CAPTURE $(fold 'length(RAMT)' 1)"
echo "# results: $OUT/results.tsv" >&2
