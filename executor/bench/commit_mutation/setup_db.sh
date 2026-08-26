#!/usr/bin/env bash
# Set up one isolated ClickHouse database with the real ROM loaded, decoded
# and bootstrapped -- the shared preamble for every arm of the #182/#180
# measurement. Deliberately a thin sequencer over the project's existing
# scripts (sqlcpu/schema.sql, sqlcpu/load_rom.py, sqlcpu/decode.sql,
# executor/bootstrap.py, rom/bench/canonical_throughput/seed_snapshot.py) --
# never a second copy of what any of them does (#101's lesson).
#
# Usage: setup_db.sh --container NAME --db DB --bin ... --manifest ...
#                    [--window boot|gameplay] [--snapshot FILE]
set -euo pipefail
cd "$(dirname "$0")/../../.."   # repo root

CONTAINER=""; DB=""; BIN="rom/build/doom-rv32im.bin"; MANIFEST="rom/build/manifest.json"
WINDOW="boot"; SNAPSHOT=""
PASSWORD="${CLICKHOUSE_PASSWORD:-clickdoom}"
while [ $# -gt 0 ]; do
  case "$1" in
    --container) CONTAINER="$2"; shift 2 ;;
    --db) DB="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    --manifest) MANIFEST="$2"; shift 2 ;;
    --window) WINDOW="$2"; shift 2 ;;
    --snapshot) SNAPSHOT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
if [ -z "$CONTAINER" ] || [ -z "$DB" ]; then
  echo "::error::--container and --db are required" >&2
  exit 1
fi

CLIENT="docker exec -i $CONTAINER clickhouse-client"
ch() { local db="$1"; shift; $CLIENT --host localhost --port 9000 --user default \
        --database "$db" --password "$PASSWORD" "$@"; }

# ROM hash gate: every number must trace to rom/PINNED_HASH's binary.
ROM_SHA=$(shasum -a 256 "$BIN" | awk '{print $1}')
PINNED=$(cat rom/PINNED_HASH)
[ "$ROM_SHA" = "$PINNED" ] || { echo "::error::$BIN sha256 $ROM_SHA != PINNED_HASH $PINNED" >&2; exit 1; }

TEXT_START=$(python3 -c "import json;print(json.load(open('$MANIFEST'))['text_start'])")
TEXT_END=$(python3 -c "import json;print(json.load(open('$MANIFEST'))['text_end'])")
TEXT_START_WORD=$(( TEXT_START / 4 ))
TEXT_END_WORD=$(( TEXT_END / 4 ))

ch default --query "DROP DATABASE IF EXISTS $DB"
sed "s/clickdoom/$DB/g" sqlcpu/schema.sql | ch default --multiquery
python3 sqlcpu/load_rom.py --bin "$BIN" --manifest "$MANIFEST" --host localhost --port 9000 \
    --user default --password "$PASSWORD" --database "$DB" --client "$CLIENT" >&2
sed "s/clickdoom/$DB/g" sqlcpu/decode.sql | \
    ch "$DB" --multiquery --param_text_start_word="$TEXT_START_WORD" --param_text_end_word="$TEXT_END_WORD"
python3 executor/bootstrap.py --host localhost --port 9000 --user default \
    --password "$PASSWORD" --database "$DB" --client "$CLIENT" >&2

if [ "$WINDOW" = "gameplay" ]; then
  if [ -z "$SNAPSHOT" ] || [ ! -f "$SNAPSHOT" ]; then
    echo "::error::--snapshot FILE required for --window gameplay" >&2
    exit 1
  fi
  ch "$DB" --query "TRUNCATE TABLE ram"
  python3 rom/bench/canonical_throughput/seed_snapshot.py --snapshot "$SNAPSHOT" \
      --host localhost --port 9000 --user default --password "$PASSWORD" \
      --database "$DB" --client "$CLIENT" >&2
fi

# Density/provenance readout -- the numbers the report has to cite.
printf 'db\t%s\n' "$DB" >&2
printf 'ram_rows\t%s\n' "$(ch "$DB" --query 'SELECT count() FROM ram')" >&2
printf 'ram_rows_final\t%s\n' "$(ch "$DB" --query 'SELECT count() FROM ram FINAL')" >&2
printf 'decoded_rows\t%s\n' "$(ch "$DB" --query 'SELECT count() FROM decoded')" >&2
printf 'rom_sha256\t%s\n' "$ROM_SHA" >&2
