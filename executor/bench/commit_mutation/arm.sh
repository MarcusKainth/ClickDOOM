#!/usr/bin/env bash
# Run ONE arm of the #182/#180 measurement in a genuinely fresh, isolated
# ClickHouse container.
#
# Fresh *container*, not a fresh database: ClickHouse's compiled-expression
# cache is server-global and keyed by each island's ActionsDAG (#166), so a
# new database defeats nothing. This matters here specifically and
# verifiably -- `fold.build_step()`'s output is byte-identical for K=2,000
# and K=60,000 (checked: 55,295 chars both, `==` True), because K only
# reaches `range(K)` outside the lambda. So every arm of a K-sweep shares
# one cache key, and running them against one server would let the first
# arm warm the cache for all the others.
#
# It also prints the idle-core headroom it observed, because that -- not
# container separation -- is the condition that makes a timing safe (#164).
#
# Usage: arm.sh --label NAME [--container NAME] [--window boot|gameplay]
#               [--snapshot FILE] [-- <extra bench.py args>]
set -euo pipefail
cd "$(dirname "$0")/../../.."   # repo root

LABEL=""; CONTAINER="sq2-arm-ch"; WINDOW="boot"; SNAPSHOT=""; OUTDIR="${SQ2_OUTDIR:-/tmp/sq2-bench}"
IMAGE="clickhouse/clickhouse-server:26.7.5.10@sha256:800e82865530eb2f1c4bc1b960e43b435fd9b2d83b4bd04a2564a5cfd88fdb6e"   # the pin from docker-compose.yml
PASSWORD="${CLICKHOUSE_PASSWORD:-clickdoom}"
EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    --label) LABEL="$2"; shift 2 ;;
    --container) CONTAINER="$2"; shift 2 ;;
    --window) WINDOW="$2"; shift 2 ;;
    --snapshot) SNAPSHOT="$2"; shift 2 ;;
    --outdir) OUTDIR="$2"; shift 2 ;;
    --) shift; EXTRA=("$@"); break ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
if [ -z "$LABEL" ]; then
  echo "::error::--label is required" >&2
  exit 1
fi
mkdir -p "$OUTDIR"

# --- idle-core headroom, recorded with the number it qualifies -----------
NCPU=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
LOAD1=$(uptime | sed -E 's/.*load average[s]?: *([0-9.]+).*/\1/')
IDLE=$(python3 -c "print(f'{$NCPU - $LOAD1:.1f}')")
echo "# headroom before arm '$LABEL': load1=$LOAD1 across $NCPU cores -> ~$IDLE idle cores" >&2
python3 -c "
import sys
if $NCPU - $LOAD1 < 8:
    sys.exit('::error::only %.1f idle cores -- refusing to time anything. Retry when quiet.' % ($NCPU - $LOAD1))
"

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" -e CLICKHOUSE_PASSWORD="$PASSWORD" \
    --ulimit nofile=262144:262144 "$IMAGE" >/dev/null
for _ in $(seq 1 60); do
  if docker exec -i "$CONTAINER" clickhouse-client --password "$PASSWORD" \
       --query "SELECT 1" >/dev/null 2>&1; then break; fi
  sleep 1
done
docker exec -i "$CONTAINER" clickhouse-client --password "$PASSWORD" --query "SELECT version()" >&2

DB="bench_${WINDOW}"
SETUP=(--container "$CONTAINER" --db "$DB" --window "$WINDOW")
if [ -n "$SNAPSHOT" ]; then
  SETUP+=(--snapshot "$SNAPSHOT")
fi
./executor/bench/commit_mutation/setup_db.sh "${SETUP[@]}"

python3 executor/bench/commit_mutation/bench.py --container "$CONTAINER" --db "$DB" \
    --label "$LABEL" --out "$OUTDIR/$LABEL.json" "${EXTRA[@]}" > /dev/null

echo "# arm '$LABEL' done -> $OUTDIR/$LABEL.json (headroom ~$IDLE idle cores of $NCPU)" >&2
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
