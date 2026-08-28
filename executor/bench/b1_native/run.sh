#!/usr/bin/env bash
# B1 benchmark driver: runs the canonical throughput instrument against arms A, B and C.
# See README.md. Take the machine lock (kind: timing) before running.
# Usage: run.sh [--repeats 3] [--batches 3] [--arms ABC] [--out DIR]
# Env: CLICKDOOM_NATIVE_CLICKHOUSE (arm C binary), B1_IMAGE_A, B1_IMAGE_B (Docker images), MALLOC_CONF (passed to arm C).
set -euo pipefail
cd "$(dirname "$0")/../../.."
HERE="executor/bench/b1_native"
REPEATS=3
BATCHES=3
ARMS="ABC"
OUT="${TMPDIR:-/tmp}/clickdoom-b1-native/$(date -u +%Y%m%dT%H%M%SZ)"
NATIVE_BIN="${CLICKDOOM_NATIVE_CLICKHOUSE:-$HOME/.clickhouse/26.3.25.2/clickhouse}"
IMAGE_A="${B1_IMAGE_A:-clickhouse/clickhouse-server:26.3.17.4}"
IMAGE_B="${B1_IMAGE_B:-clickhouse/clickhouse-server:26.3.25.2}"
while [ $# -gt 0 ]; do
  case "$1" in
    --repeats) REPEATS="$2"; shift 2 ;;
    --batches) BATCHES="$2"; shift 2 ;;
    --arms) ARMS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
mkdir -p "$OUT"
CLIENT="$NATIVE_BIN client"
[ -x "$NATIVE_BIN" ] || { echo "::error::native binary missing: $NATIVE_BIN" >&2; exit 1; }
log() { echo "# $(date -u +%H:%M:%SZ) $*" | tee -a "$OUT/run.log" >&2; }

docker_fresh() {  # name image tcp_port http_port
  docker rm -f "$1" >/dev/null 2>&1 || true
  docker run -d --name "$1" --ulimit nofile=262144 -e CLICKHOUSE_PASSWORD=clickdoom \
      -p "127.0.0.1:$3:9000" -p "127.0.0.1:$4:8123" "$2" >/dev/null
  for _ in $(seq 1 100); do
    $CLIENT --host 127.0.0.1 --port "$3" --password clickdoom --query "SELECT 1" >/dev/null 2>&1 && return 0
    sleep 0.3
  done
  echo "::error::container $1 did not come up" >&2; docker logs "$1" | tail -20 >&2; exit 1
}

arm_start() {  # arm -> sets PORT CONTAINER_ENV VERSION
  case "$1" in
    A) docker_fresh b1-arm-a "$IMAGE_A" 9010 8133; PORT=9010; CONTAINER_ENV=b1-arm-a ;;
    B) docker_fresh b1-arm-b "$IMAGE_B" 9020 8143; PORT=9020; CONTAINER_ENV=b1-arm-b ;;
    C) "$HERE/native_server.sh" fresh --binary "$NATIVE_BIN" --tcp-port 9100 --http-port 8223 2>>"$OUT/run.log"; PORT=9100; CONTAINER_ENV=native ;;
    *) echo "bad arm $1" >&2; exit 1 ;;
  esac
  VERSION=$($CLIENT --host 127.0.0.1 --port "$PORT" --password clickdoom --query "SELECT version()")
}
arm_stop() {
  case "$1" in
    A) docker rm -f b1-arm-a >/dev/null 2>&1 || true ;;
    B) docker rm -f b1-arm-b >/dev/null 2>&1 || true ;;
    C) "$HERE/native_server.sh" stop ;;
  esac
}
STARTED=""
cleanup() { local a; for a in $STARTED; do arm_stop "$a"; done; }
trap cleanup EXIT

log "provenance: git=$(git rev-parse HEAD) rom=$(cut -c1-12 rom/PINNED_HASH) native_bin=$NATIVE_BIN sha256=$(shasum -a 256 "$NATIVE_BIN" | cut -d' ' -f1)"
log "provenance: image A=$(docker image inspect "$IMAGE_A" --format '{{.Id}}' 2>/dev/null || echo MISSING) B=$(docker image inspect "$IMAGE_B" --format '{{.Id}}' 2>/dev/null || echo MISSING)"
printf 'repeat\tarm\tversion\tplatform\twindow\tmode\tk\thwm\tretired\tinstr_per_sec\n' > "$OUT/results.tsv"

for r in $(seq 1 "$REPEATS"); do
  n=${#ARMS}; s=$(( (r - 1) % n )); order="${ARMS:$s}${ARMS:0:$s}"
  for (( i=0; i<${#order}; i++ )); do
    arm="${order:$i:1}"
    log "repeat $r arm $arm: starting fresh server"
    arm_start "$arm"; STARTED="$STARTED $arm"
    platform=$([ "$arm" = C ] && echo native || echo docker)
    log "repeat $r arm $arm: version=$VERSION platform=$platform port=$PORT"
    t0=$(date +%s)
    # stdin must be /dev/null: an inline INSERT via `clickhouse client --query` blocks on an open pipe.
    if CLICKDOOM_CH_CONTAINER="$CONTAINER_ENV" rom/bench/canonical_throughput/run.sh \
         --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
         --batches "$BATCHES" --host 127.0.0.1 --port "$PORT" --password clickdoom \
         --client "$CLIENT" </dev/null > "$OUT/r${r}_${arm}.tsv" 2> "$OUT/r${r}_${arm}.log"; then
      tail -n +2 "$OUT/r${r}_${arm}.tsv" | awk -v r="$r" -v a="$arm" -v v="$VERSION" -v p="$platform" \
        'BEGIN{OFS="\t"} {print r, a, v, p, $0}' >> "$OUT/results.tsv"
      log "repeat $r arm $arm: done in $(( $(date +%s) - t0 ))s"
      grep -E "instr/sec=" "$OUT/r${r}_${arm}.log" | sed 's/^/#   /' | tee -a "$OUT/run.log" >&2
    else
      log "repeat $r arm $arm: FAILED (see $OUT/r${r}_${arm}.log)"
    fi
    arm_stop "$arm"
  done
done
log "all done: $OUT/results.tsv"
cat "$OUT/results.tsv"
