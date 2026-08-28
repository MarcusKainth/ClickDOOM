#!/usr/bin/env bash
# The resumable batch-loop runner, originally built for #110's Phase 2
# milestone (DOOM reaches its first FRAME_COMMIT inside ClickHouse) and
# extended by #210 to run through every FRAME_COMMIT to a target icount in
# one invocation -- the shape Phase 3's ~2,172-frame `demo3` run needs.
# Cross-scope (`rom` building `executor`-shaped tooling), signed off by the
# team lead per CLAUDE.md's cross-scope provision -- taking the runner off
# `executor` lets executor-2 build #29 (the frame readout) in parallel
# instead of sequentially.
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
#   - Invokes the frame readout (#229) via driver/render.py's
#     frame_readout_sql()/frame_readout_fb_hash_sql(), called verbatim on
#     every has_frame=1 batch -- never a hand-rolled INSERT INTO
#     frames_out. Until #229, nothing on this run path ever wrote
#     frames_out at all.
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
# this run crosses (target icount 15,393,136 / 1,048,576 ~= 14.7) costs
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
# ## Runs through FRAME_COMMITs instead of stopping at the first one (#210)
#
# Earlier versions of this script stopped unconditionally on the first
# FRAME_COMMIT, because #110's original milestone target (icount
# 15,393,136) happened to BE the first frame's exact icount -- "reached
# target" and "hit a frame" were the same event, so the distinction was
# invisible. Past frame 0, that meant re-invoking this script once per
# intervening FRAME_COMMIT (scripts/run_milestone_through_frames.sh proved
# the shape of the fix reaching frame 25, then was deleted once this
# landed -- a stopgap kept beside the real fix drifts from it).
#
# A FRAME_COMMIT is now recorded (logged, counted in `frames_observed`) and
# the loop continues, unless `--stop-at-frame N` was given and this frame's
# `frame_no >= N`. Stop conditions are exhaustively: target icount reached,
# `--stop-at-frame` satisfied, fatal halt, SIGINT/SIGTERM -- nothing else.
# `--stop-at-frame 0` reproduces the old unconditional-first-frame behavior
# (#110's milestone); omitting it runs straight through every FRAME_COMMIT
# to the target icount, which is what a multi-day Phase 3 `demo3` run needs
# from a single invocation.
#
# fb_hash (SPEC §7) is wired into scripts/checkpoint_query.py as of #210,
# reading the real `framebuffer`/`palette` tables (#160/#174) -- all 5
# trace fields are compared at every RAM_HASH_INTERVAL boundary now, not
# just the first 4.
#
# Usage:
#   scripts/run_milestone.sh --bin rom/build/doom-rv32im.bin \
#     --manifest rom/build/manifest.json --k 60000 --hwm 20000 \
#     --database clickdoom --trace path/to/reference_trace.tsv \
#     --target-icount 15393136 \  # #175's unrolled ROM; was 15653137 before it
#     [--stop-at-frame N] \  # optional; stop cleanly once frame_no >= N
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
STOP_AT_FRAME=""
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
    --stop-at-frame) STOP_AT_FRAME="$2"; shift 2 ;;
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
# Checked assignment, not `ch --multiquery <<< "$(...)"` directly: a
# command substitution feeding a here-string discards its exit status even
# under `set -euo pipefail` (#228) -- if commit.py raises, `ch` would be
# handed empty stdin, accept it, and exit 0 as though the flush committed.
# Assigning to a variable first makes the substitution's own failure a
# checked simple command, so `set -e` fires -- same pattern as `BATCH_SQL=`
# below.
CPU_STATE_SQL=$(python3 executor/commit.py cpu_state --db "$DATABASE")
ch --multiquery <<< "$CPU_STATE_SQL"

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

# Checked assignment before the `read`, for the same reason as
# CPU_STATE_SQL above (#228) -- and worse here if skipped: `<<< "$(...)"`
# discards a failed query's exit status, so RESUME_ICOUNT would come back
# "", which bash arithmetic evaluates as 0. A transient query failure on
# resume would then silently restart a multi-day run from icount 0 instead
# of failing loudly.
RESUME_LINE=$(ch --query \
  "SELECT max(batch_id), max(icount) FROM cpu_state FINAL" | tr '\t' ' ')
read -r RESUME_BATCH RESUME_ICOUNT <<< "$RESUME_LINE"
echo "# resuming from batch_id=$RESUME_BATCH icount=$RESUME_ICOUNT" >&2
ICOUNT="$RESUME_ICOUNT"

reached_target=0
halted_reason=""
# Count of FRAME_COMMITs observed BY THIS INVOCATION only -- not carried
# over from a prior resumed run (there is nowhere durable to carry it from;
# SPEC §5's cpu_state/batch_commit have no such counter, by design -- see
# their own "one row per committed batch" contract). A resumed run's
# provenance block therefore reports how many frames this leg passed
# through, not the run's lifetime total.
frames_observed=0

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

  # Checked assignment before each flush, not `ch --multiquery <<< "$(...)"`
  # directly (#228): a command substitution feeding a here-string discards
  # its exit status even under `set -euo pipefail`, so a raising commit.py
  # would hand `ch` empty stdin -- accepted, exit 0 -- and the loop would
  # continue as though the flush committed. Assigning to a variable first
  # makes the substitution's own failure a checked simple command, so
  # `set -e` fires. Same pattern as `BATCH_SQL=` above.
  RAM_SQL=$(python3 executor/commit.py ram --db "$DATABASE")
  ch --multiquery <<< "$RAM_SQL"
  FBPAL_SQL=$(python3 executor/commit.py fbpal --db "$DATABASE")
  ch --multiquery <<< "$FBPAL_SQL"
  CONSOLE_OUT_SQL=$(python3 executor/commit.py console_out --db "$DATABASE")
  ch --multiquery <<< "$CONSOLE_OUT_SQL"
  CPU_STATE_SQL=$(python3 executor/commit.py cpu_state --db "$DATABASE")
  ch --multiquery <<< "$CPU_STATE_SQL"
  # SPEC §5: "the driver issues [retention] unconditionally every batch" --
  # every batch, not on a cadence (#193: #187 shipped a cadence gate here
  # that contradicted this line; reverted, since the throughput case for it
  # didn't clear the bar for a spec-change ratification -- retention was
  # measured at 0.05-0.13% of batch time, below the fold noise floor).
  RETENTION_SQL=$(python3 executor/commit.py retention --db "$DATABASE")
  ch --multiquery <<< "$RETENTION_SQL"

  # Never trust STEP_K as what actually retired -- a batch can stop early
  # on the write-log high-water mark, a halt, or FRAME_COMMIT. Re-read the
  # real state every iteration (this doubles as #110's external progress
  # query, run here rather than duplicated).
  read -r BATCH_ID ICOUNT PC HALTED HALT_REASON <<< "$(ch --query \
    "SELECT batch_id, icount, pc, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1" \
    | tr '\t' ' ')"
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
    EXPECTED_LINE=$(awk -F'\t' -v ic="$ICOUNT" '$1 == ic { print; found=1 } END { if (!found) exit 1 }' "$TRACE") \
      || fail "no reference trace line for icount=$ICOUNT in $TRACE -- trace/run icount cadence disagree"
    # All 5 fields now (#210): checkpoint_query.py computes fbhash from the
    # real framebuffer/palette tables (#160/#174 landed this schema), so
    # ACTUAL_LINE and the reference trace's RAM_HASH_INTERVAL rows are the
    # same shape -- no truncation needed on either side.
    if [ "$ACTUAL_LINE" != "$EXPECTED_LINE" ]; then
      fail "checkpoint mismatch at icount=$ICOUNT
  expected (icount/pc/reghash/ramhash/fbhash): $EXPECTED_LINE
  actual:                                      $ACTUAL_LINE"
    fi
    echo "# checkpoint OK at icount=$ICOUNT (icount/pc/reghash/ramhash/fbhash)" >&2
  fi

  if [ "$HALTED" = "1" ]; then
    halted_reason="$HALT_REASON"
    break
  fi
  if [ "$HAS_FRAME" = "1" ]; then
    frames_observed=$(( frames_observed + 1 ))
    # #229: invoke the frame readout itself -- until now this branch only
    # observed has_frame/frame_no and logged them; nothing ever populated
    # frames_out, so a full Phase 3 run would end with zero rows in the
    # table the Definition of Victory is defined over. render.py's own
    # frame_readout_sql() is called verbatim, unmodified -- per PURITY.md
    # the driver's job is noticing has_frame=1 and executing the SQL it's
    # handed, nothing more (#220 already made this query dense/correct;
    # #223 already made batches stop exactly on FRAME_COMMIT so this fires
    # at the right instant -- this issue is only the missing invocation).
    # Checked assignment before the `ch` call, same #228 reasoning as
    # BATCH_SQL/RAM_SQL/etc. above: a raising render.py call must be a
    # checked simple command under `set -e`, not silently-empty stdin.
    READOUT_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
import render
print(render.frame_readout_sql(db='$DATABASE'))
")
    ch --multiquery <<< "$READOUT_SQL"
    # fb_hash appended to the log line (#229's own suggestion, adopted):
    # render.frame_readout_fb_hash_sql() already exists (#220) and costs
    # nothing new to call -- SQL computes it, this just prints the scalar
    # result, same shape as the RAM_HASH_INTERVAL checkpoint line above.
    # Not compared against a per-frame reference here -- SPEC §7 has no
    # per-FRAME_COMMIT checkpoint cadence to compare against yet (only
    # CHECKPOINT_INTERVAL/RAM_HASH_INTERVAL); a live per-frame cross-engine
    # comparison needs refemu to emit one, which is a trace-format change
    # tracked separately, not folded into this invocation fix.
    FRAME_FBHASH=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
import render
print(render.frame_readout_fb_hash_sql(db='$DATABASE'))
" | ch --format TSVRaw)
    echo "# FRAME_COMMIT observed: frame_no=$FRAME_NO icount=$ICOUNT fb_hash=$FRAME_FBHASH (frames_observed=$frames_observed)" >&2
    if [ -n "$STOP_AT_FRAME" ] && [ "$FRAME_NO" -ge "$STOP_AT_FRAME" ]; then
      reached_target=1
      break
    fi
    # Otherwise: recorded above, loop continues -- FRAME_COMMIT is no
    # longer a stop condition by itself (#210).
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
printf 'frames_observed\t%s\n' "$frames_observed"
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
  echo "# stopped cleanly: icount=$ICOUNT target=$TARGET_ICOUNT stop_at_frame=${STOP_AT_FRAME:-<none>} frame_no=${FRAME_NO:-} frames_observed=$frames_observed" >&2
  exit 0
fi
