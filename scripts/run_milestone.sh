#!/usr/bin/env bash
# The resumable batch-loop runner for #110's Phase 2 milestone: DOOM
# reaches its first FRAME_COMMIT inside ClickHouse. Cross-scope (`rom`
# building `executor`-shaped tooling), signed off by the team lead per
# CLAUDE.md's cross-scope provision -- taking the runner off `executor`
# lets executor-2 build #29 (the frame readout) in parallel instead of
# sequentially.
#
# What this does, and does not, reimplement:
#   - Calls scripts/preflight_milestone.sh first and refuses to start if it
#     fails. That script owns the four pre-run gates; this one does not
#     re-derive any of them.
#   - Loops executor/fold.py's batch() then executor/commit.py's three
#     flushes (ram, console_out, cpu_state) plus retention, called exactly
#     as those modules define them -- never a hand-rolled second copy of
#     flush SQL (#101's lesson: a second copy drifts from the first).
#   - Diffs the SQL CPU's state against refemu's committed reference trace
#     at every RAM_HASH_INTERVAL checkpoint via scripts/checkpoint_query.py
#     (itself a thin wrapper around sqlcpu/checkpoint.py -- never a second
#     hash implementation). Stops on first mismatch and reports the icount.
#   - Progress is `SELECT max(batch_id), max(icount) FROM cpu_state FINAL`,
#     exactly as #110 asked -- nothing new built for it.
#   - Resumable with no snapshot file of its own: ADR-0003 already makes
#     cpu_state/batch_commit durably resumable (every commit.py flush is
#     idempotent, keyed on the latest batch_id), so "resume" is the same
#     progress query above, read once at startup.
#   - Records provenance (ROM hash, decoded row count, K, HWM, trace path)
#     on every exit path, reusing preflight's own provenance block rather
#     than a second copy of it.
#
# ## The checkpoint-cadence problem, and why K varies per batch
#
# SPEC §7's checkpoint intervals (4,096 / 1,048,576) don't divide evenly
# into any fixed K -- 1,048,576 / 60,000 ~= 17.48. A run of constant-K
# batches would almost never land a batch boundary exactly on a
# RAM_HASH_INTERVAL instruction, so there would be nothing to diff at most
# checkpoints -- a check that silently does nothing, indistinguishable from
# one that passes, is exactly this project's signature failure class.
#
# Fix: each batch() call is passed `min(K, next_boundary - current_icount)`
# instead of a constant K, so a batch shrinks to land exactly on the next
# RAM_HASH_INTERVAL multiple, then resumes at full K past it. No fold.py
# signature change -- this is entirely the runner's own choice of what K to
# pass each call. A handful of short batches near each of the ~15 boundaries
# this run crosses (target icount 15,653,137 / 1,048,576 ~= 14.9) costs
# nothing material against a 3-4 hour run.
#
# A batch can also stop early on the write-log high-water mark (or halt, or
# FRAME_COMMIT) -- it does not always reach the K it was asked for. The loop
# below never assumes it did: every iteration re-reads the ACTUAL retired
# icount from cpu_state (via the same progress query) before computing the
# next call's K, rather than tracking an assumed running total. arrayFold
# runs all K steps regardless of how many retire, so an assumption here
# would be invisible until it silently produced the wrong K downstream.
#
# ## fb_hash is deliberately out of scope
#
# See scripts/checkpoint_query.py's own docstring: FRAMEBUFFER/PALETTE SQL
# storage doesn't exist yet (#130 computes the write-log lanes, nothing
# flushes them; that flush and its table shape are executor-2/sqlcpu-2's,
# not landed). Team lead's own framing: stopping at the target icount and
# reporting is enough until #29 lands. This runner does exactly that.
#
# Usage:
#   scripts/run_milestone.sh --bin rom/build/doom-rv32im.bin \
#     --manifest rom/build/manifest.json --k 60000 --hwm 20000 \
#     --database clickdoom --trace path/to/reference_trace.tsv \
#     --target-icount 15393136 \  # #175's unrolled ROM; was 15653137 before it
#     [--host localhost --port 9000 --user default --password ... --client '...']
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=""
MANIFEST=""
K=""
HWM=""
DATABASE="clickdoom"
TRACE=""
TARGET_ICOUNT=""
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
    --target-icount) TARGET_ICOUNT="$2"; shift 2 ;;
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --client) CLIENT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

for req in BIN MANIFEST K HWM TRACE TARGET_ICOUNT; do
  if [ -z "${!req}" ]; then
    echo "::error::--${req,,} is required" >&2
    exit 1
  fi
done

RAM_HASH_INTERVAL=1048576
# #185: matches executor/config.py's BATCH_COMMIT_RETENTION_N -- reused as
# both the retention window (how far back batch_commit keeps rows) and the
# retention cadence (how often the DELETE runs), not a coincidence: the
# DELETE recomputes its own threshold fresh every call, so running it every
# Nth batch instead of every batch still enforces the identical window
# (never more than N-1 batches of extra rows accumulate) at 1/N the
# mutation count. Kept as a script-local literal, not read from config.py,
# the same way this script already treats HWM/K as its own CLI-provided
# values rather than importing executor's defaults.
RETENTION_N=16

# shellcheck disable=SC2206  # deliberate word-split, same convention as
# preflight_milestone.sh's identical CH_CMD line.
CH_CMD=($CLIENT)
ch() {
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$DATABASE")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

fail() { echo "::error::MILESTONE RUN FAILED: $1" >&2; exit 1; }

stop_requested=0
trap 'stop_requested=1' INT TERM

echo "# --- pre-flight ---------------------------------------------------" >&2
if ! ./scripts/preflight_milestone.sh --bin "$BIN" --manifest "$MANIFEST" --k "$K" --hwm "$HWM" \
    --database "$DATABASE" --trace "$TRACE" --host "$HOST" --port "$PORT" --user "$CH_USER" \
    --password "$PASSWORD" --client "$CLIENT" >&2; then
  fail "pre-flight gate did not pass -- refusing to start (see its own output above for which gate)"
fi

echo "# --- bootstrap (idempotent -- no-op if already seeded) -------------" >&2
python3 executor/bootstrap.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
    --password "$PASSWORD" --database "$DATABASE" --client "$CLIENT" >&2
ch --multiquery <<< "$(python3 executor/commit.py cpu_state --db "$DATABASE")"

TEXT_START=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_start'])")
TEXT_END=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_end'])")
LOAD_ADDR=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['load_addr'])")
TEXT_START_WORD=$(( TEXT_START / 4 ))
TEXT_END_WORD=$(( TEXT_END / 4 ))
DECN=$(( TEXT_END_WORD - TEXT_START_WORD ))
RAM_WORDS=6291456  # SPEC §2: 24 MiB / 4, same constant preflight uses

# fold.py's text_start_widx/text_end_widx are compared directly against WA
# (executor/fold.py:74's `wa_safe`), which is RAM_BASE-*relative* and
# clamped to [0, RAM_WORDS-1] -- never an absolute word address.
# TEXT_START_WORD/TEXT_END_WORD above are absolute (manifest byte offsets
# / 4), correct for preflight's own decoded.word_addr density check (that
# table IS keyed on absolute word_addr, per SPEC §2) but wrong for
# fold.batch()'s SELF_MODIFY window -- passing them there makes
# `WA >= text_start_widx` unconditionally false (WA can never reach ~536M),
# silently disabling SELF_MODIFY detection for the whole run. Caught by
# executor-2's review (#144) after the identical bug was found already
# merged in preflight_milestone.sh's own gate 4 (#146). Subtract
# RAM_BASE_WORD to get the RAM-relative window fold.py actually expects.
RAM_BASE_WORD=$(( LOAD_ADDR / 4 ))
TEXT_START_WIDX=$(( TEXT_START_WORD - RAM_BASE_WORD ))
TEXT_END_WIDX=$(( TEXT_END_WORD - RAM_BASE_WORD ))

read -r RESUME_BATCH RESUME_ICOUNT <<< "$(ch --query \
  "SELECT max(batch_id), max(icount) FROM cpu_state FINAL" | tr '\t' ' ')"
echo "# resuming from batch_id=$RESUME_BATCH icount=$RESUME_ICOUNT" >&2
ICOUNT="$RESUME_ICOUNT"

reached_target=0
halted_reason=""

while [ "$stop_requested" -eq 0 ]; do
  if [ "$ICOUNT" -ge "$TARGET_ICOUNT" ]; then
    reached_target=1
    break
  fi

  # This batch's boundary and step size (the checkpoint-cadence fix -- see
  # header comment). NEXT_BOUNDARY is the next RAM_HASH_INTERVAL multiple
  # strictly after ICOUNT; STEP_K shrinks to land exactly on it, or on
  # TARGET_ICOUNT if that comes first, whichever is closer.
  NEXT_BOUNDARY=$(( ((ICOUNT / RAM_HASH_INTERVAL) + 1) * RAM_HASH_INTERVAL ))
  STOP_AT=$(( NEXT_BOUNDARY < TARGET_ICOUNT ? NEXT_BOUNDARY : TARGET_ICOUNT ))
  STEP_K=$(( STOP_AT - ICOUNT < K ? STOP_AT - ICOUNT : K ))
  if [ "$STEP_K" -le 0 ]; then
    STEP_K="$K"
  fi

  BATCH_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.batch($STEP_K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM, db='$DATABASE'))
")
  # Via stdin, not --query: the fold's generated step is tens of thousands
  # of AST nodes as text, well past ARG_MAX -- same reasoning as every
  # other script here that touches fold.py's output.
  echo "$BATCH_SQL" | ch --multiquery

  ch --multiquery <<< "$(python3 executor/commit.py ram --db "$DATABASE")"
  ch --multiquery <<< "$(python3 executor/commit.py fbpal --db "$DATABASE")"
  ch --multiquery <<< "$(python3 executor/commit.py console_out --db "$DATABASE")"
  ch --multiquery <<< "$(python3 executor/commit.py cpu_state --db "$DATABASE")"

  # Never trust STEP_K as what actually retired -- a batch can stop early
  # on the write-log high-water mark, a halt, or FRAME_COMMIT. Re-read the
  # real state every iteration (this doubles as #110's external progress
  # query, run here rather than duplicated).
  read -r BATCH_ID ICOUNT PC HALTED HALT_REASON <<< "$(ch --query \
    "SELECT batch_id, icount, pc, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1" \
    | tr '\t' ' ')"

  # #185: retention every RETENTION_N-th batch, not every batch -- the
  # window it enforces (batch_id > max(batch_id) - RETENTION_N) is
  # identical either way, since the DELETE recomputes that threshold fresh
  # from the CURRENT max(batch_id) whenever it runs; only the mutation
  # COUNT drops, to 1/RETENTION_N. Gated on $BATCH_ID, which SQL (the read
  # above) already computed -- same shape as the existing
  # `ICOUNT % RAM_HASH_INTERVAL` checkpoint-cadence gate a few lines below,
  # not a new kind of decision introduced here.
  if [ "$((BATCH_ID % RETENTION_N))" -eq 0 ]; then
    ch --multiquery <<< "$(python3 executor/commit.py retention --db "$DATABASE")"
  fi
  # FRAME_COMMIT is a batch-early-exit condition (SPEC §6), not a fatal
  # halt (SPEC §1's halt-reason vocabulary doesn't include it) -- cpu_state
  # has no has_frame/frame_no column (SPEC §5), only batch_commit does, so
  # this is a separate read from the halted/halt_reason one above.
  read -r HAS_FRAME FRAME_NO <<< "$(ch --query \
    "SELECT has_frame, frame_no FROM batch_commit ORDER BY batch_id DESC LIMIT 1" \
    | tr '\t' ' ')"
  echo "# batch_id=$BATCH_ID icount=$ICOUNT pc=$PC halted=$HALTED halt_reason=$HALT_REASON has_frame=$HAS_FRAME frame_no=$FRAME_NO" >&2

  if [ "$((ICOUNT % RAM_HASH_INTERVAL))" -eq 0 ] && [ "$ICOUNT" -gt 0 ]; then
    # --format TSVRaw, not the default TabSeparated: the checkpoint line is
    # ONE string value with embedded literal tab bytes (checkpoint.py's
    # concat(...)), and default TabSeparated *escapes* embedded tabs inside
    # a string value as the two characters `\`+`t` rather than passing the
    # real 0x09 byte through -- confirmed by hand (`od -c`) before trusting
    # it, since a silent escaping mismatch here would make every checkpoint
    # compare unequal to the reference trace regardless of whether the
    # underlying hash matched. TSVRaw passes the byte through unescaped.
    ACTUAL_LINE=$(ch --format TSVRaw <<< "$(python3 scripts/checkpoint_query.py --db "$DATABASE")")
    EXPECTED_FULL=$(awk -F'\t' -v ic="$ICOUNT" '$1 == ic { print; found=1 } END { if (!found) exit 1 }' "$TRACE") \
      || fail "no reference trace line for icount=$ICOUNT in $TRACE -- trace/run icount cadence disagree"
    # Compare only the first 4 fields (icount/pc/reghash/ramhash), not the
    # whole line: checkpoint_query.py deliberately doesn't compute fbhash
    # (see its own docstring -- FRAMEBUFFER/PALETTE storage doesn't exist
    # yet, #130/#29), so ACTUAL_LINE is always 4 columns. The reference
    # trace's RAM_HASH_INTERVAL rows are always 5 (checkpoint.py's own
    # format_checkpoint(): "fbhash only ever alongside ramhash"). A raw
    # whole-line compare would therefore fail at EVERY real checkpoint
    # boundary regardless of whether the CPU state actually matched --
    # caught by hand before this ever ran against a real boundary, via
    # `awk -F'\t' '$1==1048576{print NF}'` on the real trace file (prints
    # 5, not 4). `cut -f1-4` drops fbhash from the expected side so the
    # two sides are the same shape without discarding any field this
    # runner *can* verify.
    EXPECTED_LINE=$(printf '%s' "$EXPECTED_FULL" | cut -f1-4)
    if [ "$ACTUAL_LINE" != "$EXPECTED_LINE" ]; then
      fail "checkpoint mismatch at icount=$ICOUNT
  expected (icount/pc/reghash/ramhash): $EXPECTED_LINE
  actual:                               $ACTUAL_LINE"
    fi
    echo "# checkpoint OK at icount=$ICOUNT (icount/pc/reghash/ramhash; fbhash not checked -- #29)" >&2
  fi

  if [ "$HALTED" = "1" ]; then
    halted_reason="$HALT_REASON"
    break
  fi
  if [ "$HAS_FRAME" = "1" ]; then
    echo "# FRAME_COMMIT observed: frame_no=$FRAME_NO icount=$ICOUNT" >&2
    reached_target=1
    break
  fi
done

echo "" >&2
echo "# --- run provenance -----------------------------------------------" >&2
PINNED=$(cat rom/PINNED_HASH)
printf 'rom_sha256\t%s\n' "$PINNED"
printf 'decoded_rows\t%s\n' "$DECN"
printf 'K\t%s\n' "$K"
printf 'HWM\t%s\n' "$HWM"
printf 'reference_trace\t%s\n' "$TRACE"
printf 'database\t%s\n' "$DATABASE"
printf 'final_batch_id\t%s\n' "${BATCH_ID:-$RESUME_BATCH}"
printf 'final_icount\t%s\n' "$ICOUNT"
printf 'has_frame\t%s\n' "${HAS_FRAME:-0}"
printf 'frame_no\t%s\n' "${FRAME_NO:-}"
echo "# ---------------------------------------------------------------------" >&2

if [ "$stop_requested" -eq 1 ]; then
  echo "# stopped: SIGINT/SIGTERM received, current batch's flush already committed -- safe to resume" >&2
  exit 0
elif [ -n "$halted_reason" ]; then
  # A fatal halt short of the target/first frame is a real failure to
  # report loudly, not swallow as a clean stop.
  echo "# stopped: halted, reason=$halted_reason, icount=$ICOUNT" >&2
  if [ "$ICOUNT" -ge "$TARGET_ICOUNT" ]; then
    exit 0
  fi
  fail "fatal halt ($halted_reason) at icount=$ICOUNT, short of target icount=$TARGET_ICOUNT"
elif [ "$reached_target" -eq 1 ]; then
  echo "# stopped cleanly: icount=$ICOUNT target=$TARGET_ICOUNT has_frame=${HAS_FRAME:-0}" >&2
  exit 0
fi
