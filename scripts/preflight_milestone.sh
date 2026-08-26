#!/usr/bin/env bash
# Pre-flight gate for a multi-hour run against the real ROM (#110's Phase 2
# milestone first; any later multi-hour run -- demo3 included -- should call
# this too). Refuses to start rather than advising: every check below is
# prose in #110 today, which means it is advice, and advice gets skipped at
# hour zero of a 2.8-hour run by someone in a hurry. This script can't be
# skipped that way -- it exits nonzero and the run doesn't begin.
#
# Four gates (#110's own list):
#   1. `decoded` is populated and the right size -- an empty or short
#      `decoded` does not error, it silently executes no-ops and reports
#      *flattering* throughput (#83). This is the one that would waste the
#      entire run and hand back a plausible number at the end of it.
#   2. `ram` is dense over SPEC §2's full 24 MiB (#81) -- `load_rom.py`
#      asserts this at load time already; this re-checks rather than trust
#      provisioning order, same reasoning as #93's density guard.
#   3. The loaded ROM binary matches `rom/PINNED_HASH` exactly -- `main` has
#      carried four ROMs in one day; a run against the wrong one produces an
#      `fb_hash` mismatch at hour three that looks like a divergence bug,
#      not a stale binary.
#   4. A real, isolated smoke-test batch actually retires what it's asked to
#      retire, run against a THROWAWAY database seeded with the SAME loaded
#      ROM state, before the real run is trusted to go unattended for hours.
#      `arrayFold` runs all K steps regardless of how many retire, so a
#      silently-stalling batch is indistinguishable from a working one by
#      wall clock alone -- this catches that class of failure before the
#      real run's first batch, not after its 221st.
#
# Applying #98's own review question to every check here: can two errors
# cancel and make it pass anyway? That question is what found the
# count()-vs-count(DISTINCT) hole on `decoded`'s density guard -- gate 1
# below uses count(DISTINCT word_addr), not bare count(), for exactly that
# reason: `decoded` is a plain MergeTree with no per-key dedup, unlike `ram`
# (ReplacingMergeTree, read with FINAL below, where count() is provably
# equivalent -- see gate 2's comment).
#
# Emits the run's provenance (ROM hash, decoded row count, K, HWM, reference
# trace path) on success, so the eventual result carries its own context.
#
# Usage:
#   scripts/preflight_milestone.sh --bin rom/build/doom-rv32im.bin \
#     --manifest rom/build/manifest.json --k 60000 --hwm 20000 \
#     [--database clickdoom] [--trace path/to/reference_trace.tsv] \
#     [--host localhost] [--port 9000] [--user default] [--password ...] \
#     [--client 'clickhouse-client']
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=""
MANIFEST=""
K=""
HWM=""
DATABASE="clickdoom"
TRACE=""
HOST="localhost"
PORT="9000"
CH_USER="default"
PASSWORD="${CLICKHOUSE_PASSWORD:-}"
CLIENT="clickhouse-client"

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --manifest) MANIFEST="$2"; shift 2 ;;
    --k) K="$2"; shift 2 ;;
    --hwm) HWM="$2"; shift 2 ;;
    --database) DATABASE="$2"; shift 2 ;;
    --trace) TRACE="$2"; shift 2 ;;
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --client) CLIENT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

for req in BIN MANIFEST K HWM; do
  if [ -z "${!req}" ]; then
    echo "::error::--${req,,} is required" >&2
    exit 1
  fi
done

# shellcheck disable=SC2206  # deliberate word-split: --client "docker exec -i
# clickdoom-ch clickhouse-client" (multiple argv words) is a supported form,
# same convention as sqlcpu/load_rom.py's/executor/fold.py's --client.
CH_CMD=($CLIENT)
ch() {
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$DATABASE")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

fail() { echo "::error::PRE-FLIGHT FAILED: $1" >&2; exit 1; }

echo "# pre-flight: gate 1/4 -- decoded populated and correctly sized" >&2
TEXT_START=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_start'])")
TEXT_END=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_end'])")
TEXT_START_WORD=$(( TEXT_START / 4 ))
TEXT_END_WORD=$(( TEXT_END / 4 ))
EXPECTED_DECODED=$(( TEXT_END_WORD - TEXT_START_WORD ))

# count(DISTINCT word_addr), not count(): `decoded` is plain MergeTree, no
# per-key dedup -- a duplicate row at one address and a genuine gap at
# another can cancel out in a bare count() (#98's finding, applied here
# rather than re-derived). min/max close the remaining gap: count(DISTINCT)
# = expected width with matching endpoints is airtight by pigeonhole --
# expected_count distinct integers spanning exactly [lo, hi] can only be the
# full contiguous range.
read -r DEC_CNT DEC_MIN DEC_MAX <<< "$(ch --query \
  "SELECT count(DISTINCT word_addr), min(word_addr), max(word_addr) FROM decoded" | tr '\t' ' ')"
if [ "$DEC_CNT" -eq 0 ]; then
  fail "decoded is EMPTY (0 rows) -- this is #83's exact failure shape: the run would execute K no-ops per batch, retire nothing real, and report throughput that looks fine. Run sqlcpu/decode.sql before retrying."
fi
if [ "$DEC_CNT" -ne "$EXPECTED_DECODED" ] || [ "$DEC_MIN" -ne "$TEXT_START_WORD" ] || [ "$DEC_MAX" -ne $(( TEXT_END_WORD - 1 )) ]; then
  fail "decoded is not dense over [text_start_word=$TEXT_START_WORD, text_end_word=$TEXT_END_WORD): got count(DISTINCT)=$DEC_CNT min=$DEC_MIN max=$DEC_MAX, expected count=$EXPECTED_DECODED min=$TEXT_START_WORD max=$(( TEXT_END_WORD - 1 )). A short/sparse decoded table silently no-ops the tail of the program while executing the head correctly -- harder to spot than all-zeros (#83)."
fi
echo "  decoded: $DEC_CNT rows, word_addr $DEC_MIN..$DEC_MAX -- OK" >&2

echo "# pre-flight: gate 2/4 -- ram dense over SPEC §2's 24 MiB" >&2
RAM_BASE=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['load_addr'])")
RAM_BASE_WORD=$(( RAM_BASE / 4 ))
RAM_WORDS=6291456  # SPEC §2: 24 MiB / 4

# count(), not count(DISTINCT), IS airtight here -- unlike decoded above,
# `ram` is ReplacingMergeTree and this reads it FINAL, which guarantees at
# most one row per word_addr. count() after FINAL and count(DISTINCT
# word_addr) are provably the same number for this table; load_rom.py's own
# load-time check (sqlcpu/load_rom.py) uses the same count()+span+min form
# for the same reason. Re-checking here rather than trusting that load
# happened, or happened last, per the team lead's ask.
read -r RAM_CNT RAM_MIN RAM_MAX <<< "$(ch --query \
  "SELECT count(), min(word_addr), max(word_addr) FROM ram FINAL" | tr '\t' ' ')"
if [ "$RAM_CNT" -ne "$RAM_WORDS" ] || [ "$RAM_MIN" -ne "$RAM_BASE_WORD" ] || [ "$RAM_MAX" -ne $(( RAM_BASE_WORD + RAM_WORDS - 1 )) ]; then
  fail "ram is not dense over SPEC §2's 24 MiB region: got count=$RAM_CNT min=$RAM_MIN max=$RAM_MAX, expected count=$RAM_WORDS min=$RAM_BASE_WORD max=$(( RAM_BASE_WORD + RAM_WORDS - 1 )). RAMT indexes positionally (#81) -- a sparse ram silently displaces every load past the first gap, no halt, no error."
fi
echo "  ram: $RAM_CNT rows, word_addr $RAM_MIN..$RAM_MAX -- OK" >&2

echo "# pre-flight: gate 3/4 -- ROM binary matches rom/PINNED_HASH" >&2
PINNED=$(cat rom/PINNED_HASH)
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$BIN" | cut -d' ' -f1)
else
  ACTUAL=$(shasum -a 256 "$BIN" | cut -d' ' -f1)
fi
MANIFEST_SHA=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['sha256'])")
if [ "$ACTUAL" != "$PINNED" ]; then
  fail "$BIN sha256 ($ACTUAL) != rom/PINNED_HASH ($PINNED). main has carried multiple ROMs in one day -- a run against the wrong binary produces an fb_hash mismatch hours in that looks like a CPU divergence, not a stale artifact. Rebuild (just build-rom) or check out the right commit."
fi
if [ "$ACTUAL" != "$MANIFEST_SHA" ]; then
  fail "$BIN sha256 ($ACTUAL) != $MANIFEST's own sha256 field ($MANIFEST_SHA) -- the binary and its manifest were built at different times and don't belong together, even though the binary happens to match PINNED_HASH."
fi
echo "  rom: $ACTUAL == PINNED_HASH == manifest.sha256 -- OK" >&2

echo "# pre-flight: gate 4/4 -- smoke-test batch retires what it's asked to" >&2
# Isolated throwaway database, seeded from the SAME loaded ram/decoded state
# via a table-to-table copy (no re-decoding, no re-loading -- this is meant
# to prove the CURRENT state actually works, not a fresh one). A small K
# (min(1000, the real run's K) instructions) run through fold.py's own
# select_only(), the same code path -- not a hand-rolled query -- the real
# run's batches will execute.
SMOKE_DB="clickdoom_preflight_smoke_$$"
SMOKE_K=$(( K < 1000 ? K : 1000 ))
ch --query "DROP DATABASE IF EXISTS $SMOKE_DB"
ch --query "CREATE DATABASE $SMOKE_DB"
# `AS <source>` copies the source table's own DDL (columns AND engine)
# verbatim -- no ENGINE override needed, and none given, so this can't drift
# from whatever sqlcpu/schema.sql actually declares.
ch --query "CREATE TABLE $SMOKE_DB.ram AS $DATABASE.ram"
ch --query "CREATE TABLE $SMOKE_DB.decoded AS $DATABASE.decoded"
# decode_with()'s KEYQT subquery reads {db}.input_queue unconditionally
# (#88's MMIO plumbing) -- missing this table isn't a "no events queued"
# case, it's an UNKNOWN_TABLE query failure (the exact gap #120 found in
# executor/bench/hwm/gen.py and executor/schema_fixture.sql). Empty is the
# correct content for a smoke test; the table just has to exist.
ch --query "CREATE TABLE $SMOKE_DB.input_queue AS $DATABASE.input_queue"
ch --query "INSERT INTO $SMOKE_DB.ram SELECT * FROM $DATABASE.ram"
ch --query "INSERT INTO $SMOKE_DB.decoded SELECT * FROM $DATABASE.decoded"

cleanup_smoke() { ch --query "DROP DATABASE IF EXISTS $SMOKE_DB" 2>/dev/null || true; }
trap cleanup_smoke EXIT

if [ ! -f executor/fold.py ]; then
  fail "executor/fold.py not found -- gate 4 needs the real fold code path, not a hand-rolled substitute"
fi
# RAM-relative, not absolute: fold.py's build_step() compares
# text_start_widx/text_end_widx directly against WA (executor/fold.py:539,
# the SELF_MODIFY arm), and WA (_addr_and_align's wa_safe, fold.py:74) is
# RAM_BASE-relative -- least(bitShiftRight(toUInt32(toUInt64(ADDR) -
# RAM_BASE), 2), RAM_WORDS - 1), always in [0, RAM_WORDS - 1]. Passing the
# absolute TEXT_START_WORD/TEXT_END_WORD (gate 1's manifest-derived word
# addresses, ~536,870,912 for the current ROM) here makes `WA >=
# text_start_widx` unconditionally false for every address this fold can
# ever compute -- SELF_MODIFY can never fire, not rarely, algebraically
# (#146). Subtract RAM_BASE_WORD (gate 2) to match every other caller's
# convention (test_fold.py, fold.py's own docstrings).
TEXT_START_WIDX=$(( TEXT_START_WORD - RAM_BASE_WORD ))
TEXT_END_WIDX=$(( TEXT_END_WORD - RAM_BASE_WORD ))
FOLD_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.select_only($SMOKE_K, $TEXT_START_WIDX, $TEXT_END_WIDX, $EXPECTED_DECODED, $RAM_WORDS, $HWM,
                        pc0=$RAM_BASE, db='$SMOKE_DB'))
")
# Via stdin, not `--query "$FOLD_SQL"`: the fold's generated step expression
# is tens of thousands of AST nodes as text (#124's ~59,900-node figure), and
# passing that as a command-line argument blows past the OS's ARG_MAX
# ("argument list too long") well before it's anywhere near ClickHouse's own
# limits -- the same reason every other script here that touches fold.py's
# output (executor/bench/*/run.sh, sqlcpu/run_tests.sh) pipes SQL in rather
# than passing it as an argument.
SMOKE_OUT=$(echo "$FOLD_SQL" | ch --format TSVWithNames)
# select_only()'s column order (executor/fold.py): pc, regs, wl_addr,
# wl_val, wl_icount, stopped, halted, halt_reason, halt_pc, halt_extra,
# retired, console_bytes, keyq_pos, frame_no, frame_committed -- retired is
# field 11, halted is field 7, taken from the real SELECT list rather than
# assumed positionally.
SMOKE_ICOUNT=$(echo "$SMOKE_OUT" | tail -1 | awk -F'\t' '{print $11}')
SMOKE_HALTED=$(echo "$SMOKE_OUT" | tail -1 | awk -F'\t' '{print $7}')
if [ "$SMOKE_HALTED" = "0" ] && [ "$SMOKE_ICOUNT" != "$SMOKE_K" ]; then
  fail "smoke-test batch (K=$SMOKE_K) retired $SMOKE_ICOUNT, not $SMOKE_K, and did not halt -- this is exactly the silent-stall shape #110 warns about: arrayFold ran all $SMOKE_K steps regardless, so a working run and a stalled one look identical by wall clock alone. Something in the currently-loaded state or fold path is producing an unexplained short retirement."
fi
echo "  smoke test: K=$SMOKE_K retired=$SMOKE_ICOUNT halted=$SMOKE_HALTED -- OK" >&2

echo "" >&2
echo "# ALL 4 PRE-FLIGHT GATES PASSED" >&2
echo "" >&2
echo "# --- run provenance -----------------------------------------------" >&2
printf 'rom_sha256\t%s\n' "$ACTUAL"
printf 'decoded_rows\t%s\n' "$DEC_CNT"
printf 'K\t%s\n' "$K"
printf 'HWM\t%s\n' "$HWM"
printf 'reference_trace\t%s\n' "${TRACE:-<none given>}"
printf 'database\t%s\n' "$DATABASE"
echo "# ---------------------------------------------------------------------" >&2
