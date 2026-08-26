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

# #140: gate 4 already requests `--format TSVWithNames` (a header row), but
# every caller used to throw it away with `tail -1` and index columns by
# hardcoded position -- the exact failure shape gate 1 exists to prevent one
# level up (#98's count()-vs-count(DISTINCT) hole): if fold.py's SELECT list
# ever reorders, a positional read silently returns the WRONG field instead
# of failing, and this gate protects a 3-4 hour run (#110) on the strength
# of that read. Extract by the header's own column name instead -- a reorder
# then fails loudly ("column not found"), which is the correct behavior for
# a guard.
tsv_field() { # tsv_field <column-name> <TSVWithNames output>
  local name="$1" data="$2" header idx
  header=$(echo "$data" | head -1)
  idx=$(echo "$header" | tr '\t' '\n' | grep -nx "$name" | cut -d: -f1)
  [ -n "$idx" ] || fail "column '$name' not found in select_only()'s TSVWithNames header ($header) -- fold.py's SELECT list changed shape; this gate's assumptions about it are stale (#140)."
  echo "$data" | tail -1 | awk -F'\t' -v i="$idx" '{print $i}'
}

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

SELFMOD_DB="clickdoom_preflight_selfmod_$$"
cleanup_smoke() {
  ch --query "DROP DATABASE IF EXISTS $SMOKE_DB" 2>/dev/null || true
  ch --query "DROP DATABASE IF EXISTS $SELFMOD_DB" 2>/dev/null || true
}
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
# By column NAME (#140), not hardcoded position -- select_only()'s SELECT
# list (executor/fold.py) aliases these as `retired`/`halted` regardless of
# where they fall in the column order.
SMOKE_ICOUNT=$(tsv_field retired "$SMOKE_OUT")
SMOKE_HALTED=$(tsv_field halted "$SMOKE_OUT")
if [ "$SMOKE_HALTED" = "0" ] && [ "$SMOKE_ICOUNT" != "$SMOKE_K" ]; then
  fail "smoke-test batch (K=$SMOKE_K) retired $SMOKE_ICOUNT, not $SMOKE_K, and did not halt -- this is exactly the silent-stall shape #110 warns about: arrayFold ran all $SMOKE_K steps regardless, so a working run and a stalled one look identical by wall clock alone. Something in the currently-loaded state or fold path is producing an unexplained short retirement."
fi
echo "  smoke test: K=$SMOKE_K retired=$SMOKE_ICOUNT halted=$SMOKE_HALTED -- OK" >&2

echo "# pre-flight: gate 4/4 (cont'd) -- SELF_MODIFY guard actually fires" >&2
# #146: the retirement check above cannot catch a unit-confusion bug in
# TEXT_START_WIDX/TEXT_END_WIDX -- the real loaded ROM's boot slice never
# performs a genuine self-modifying store within SMOKE_K instructions, so
# "SELF_MODIFY never fires" looks identical whether the arm is wired
# correctly or silently dead (which #146 proved it was: the absolute word
# addresses this gate passed before the fix made `WA >= text_start_widx`
# unconditionally false, for every address the fold can ever compute).
# Two checks close that gap, at different cost/precision points:
#
# (a) A direct bound check on THIS SCRIPT's own TEXT_START_WIDX/
#     TEXT_END_WIDX -- the exact values the real smoke test above just used
#     -- against RAM_WORDS. Free (no ClickHouse round-trip) and catches
#     #146's actual numbers precisely: 536,870,912 is not < RAM_WORDS
#     (6,291,456), so this fails immediately and loudly on the pre-fix
#     values, the same invariant executor/fold.py's build_step() now
#     enforces internally (defense in depth: that assertion already fired,
#     inside the smoke-test FOLD_SQL construction above, if these were
#     wrong -- this restates it here, closer to the values themselves, so
#     the failure message names the right gate).
if [ "$TEXT_START_WIDX" -lt 0 ] || [ "$TEXT_END_WIDX" -le "$TEXT_START_WIDX" ] || [ "$TEXT_END_WIDX" -gt "$RAM_WORDS" ]; then
  fail "TEXT_START_WIDX=$TEXT_START_WIDX/TEXT_END_WIDX=$TEXT_END_WIDX are not a valid RAM-relative range within RAM_WORDS=$RAM_WORDS -- this is #146's exact failure shape: these values are compared directly against WA, which is always RAM_BASE-relative, so anything outside [0, RAM_WORDS] here (an absolute word address, for instance) silently disables SELF_MODIFY detection for the entire real run."
fi
# (b) A synthetic self-modifying store, seeded into its OWN tiny throwaway
#     database (never the real loaded ram/decoded -- must never perturb the
#     state the run above just proved works), proving the SELF_MODIFY
#     mechanism itself still fires end-to-end -- WA's computation, the halt
#     arm's wiring, halt_reason reaching the output -- not just that the
#     bound check above passed. Same shape as executor/tests/test_fold.py's
#     test_halt_self_modify (word 0 relative = `sw x0, RAM_BASE(x0)`).
#
#     This targets word 0 relative to RAM_BASE, which only lands inside
#     [TEXT_START_WIDX, TEXT_END_WIDX) if TEXT_START_WIDX is 0 -- true for
#     every ROM this project has built (rom's linker places .text at
#     load_addr; #144's manifest check confirms load_addr == text_start).
#     Guarded explicitly rather than assumed silently, so a future ROM
#     that breaks this assumption fails loudly here instead of this check
#     quietly proving nothing.
if [ "$TEXT_START_WIDX" -ne 0 ]; then
  fail "TEXT_START_WIDX=$TEXT_START_WIDX, expected 0 -- the SELF_MODIFY synthetic check below targets word 0 relative to RAM_BASE and needs TEXT_START_WIDX=0 for that to land inside the text window. Every ROM built so far has load_addr == text_start (#144); if this ROM genuinely doesn't, this check needs to target TEXT_START_WIDX's own word instead of assuming 0, not skip silently."
fi
SM_DECN=8
SM_RAM_WORDS=8
ch --query "DROP DATABASE IF EXISTS $SELFMOD_DB"
ch --query "CREATE DATABASE $SELFMOD_DB"
ch --query "CREATE TABLE $SELFMOD_DB.decoded AS $DATABASE.decoded"
ch --query "CREATE TABLE $SELFMOD_DB.ram AS $DATABASE.ram"
ch --query "CREATE TABLE $SELFMOD_DB.input_queue AS $DATABASE.input_queue"
# word 0 (relative to RAM_BASE): op_id=19 is a store (sqlcpu/schema.sql's
# decoded.id convention), width_mask=0xFFFFFFFF (full word), sign_bit=0,
# imm=RAM_BASE -- store the word AT RAM_BASE, i.e. at itself. Words 1..7
# are op_id=31 (OP_ILLEGAL, executor/config.py) padding, never reached:
# K=1 halts on the very first step if the guard fires correctly.
SM_DEC_ROWS="($RAM_BASE_WORD,19,0,0,0,$RAM_BASE,0,4294967295,0,0)"
for sm_i in $(seq 1 $(( SM_DECN - 1 ))); do
  SM_DEC_ROWS="$SM_DEC_ROWS,($(( RAM_BASE_WORD + sm_i )),31,0,0,0,0,0,0,0,$(( 0xBAD00000 + sm_i )))"
done
ch --query "INSERT INTO $SELFMOD_DB.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) VALUES $SM_DEC_ROWS"
ch --query "INSERT INTO $SELFMOD_DB.ram (word_addr,value,version) SELECT $RAM_BASE_WORD + number, 0, 0 FROM numbers($SM_RAM_WORDS)"
# TEXT_START_WIDX is 0 here (guarded above); the synthetic end bound is
# just this tiny window's own size, well within [0, SM_RAM_WORDS].
SELFMOD_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.select_only(1, $TEXT_START_WIDX, $SM_DECN, $SM_DECN, $SM_RAM_WORDS, $HWM,
                        pc0=$RAM_BASE, db='$SELFMOD_DB'))
")
SELFMOD_OUT=$(echo "$SELFMOD_SQL" | ch --format TSVWithNames)
# By name (#140), same as the smoke-test check above.
SELFMOD_HALTED=$(tsv_field halted "$SELFMOD_OUT")
SELFMOD_REASON=$(tsv_field halt_reason "$SELFMOD_OUT")
SELFMOD_RETIRED=$(tsv_field retired "$SELFMOD_OUT")
HALT_SELF_MODIFY=$(python3 -c "import sys; sys.path.insert(0, 'executor'); import config; print(config.HALT_SELF_MODIFY)")
if [ "$SELFMOD_HALTED" != "1" ] || [ "$SELFMOD_REASON" != "$HALT_SELF_MODIFY" ]; then
  fail "SELF_MODIFY guard did not fire on a synthetic self-modifying store at word 0 relative to TEXT_START_WIDX=$TEXT_START_WIDX (got halted=$SELFMOD_HALTED halt_reason=$SELFMOD_REASON retired=$SELFMOD_RETIRED, expected halted=1 halt_reason=$HALT_SELF_MODIFY) -- the SELF_MODIFY mechanism itself (WA computation, the halt arm, or halt_reason wiring) is not working, independent of #146's widx-bound issue which check (a) above already cleared."
fi
echo "  SELF_MODIFY guard: bound check OK, synthetic store halted=$SELFMOD_HALTED halt_reason=$SELFMOD_REASON -- OK" >&2

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
