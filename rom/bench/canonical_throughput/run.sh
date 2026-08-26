#!/usr/bin/env bash
# The canonical real-ROM throughput benchmark -- built for the upcoming
# 5-day optimisation sprint (#147's ruling on #130's regression: not a
# stop, a time-boxed sprint), per team lead's explicit ask. Every
# measurement this project has taken ad hoc has cost a re-run at least
# once -- contaminated by a concurrent process, taken at a non-optimal K,
# taken against a stale ROM, taken on a synthetic fixture that mispredicted
# the real ROM's behaviour by the wrong sign. A sprint whose dozen
# experiments are each measured differently cannot be summed. This script
# is the one instrument every sprint number should come from.
#
# ## The two windows, and why one number is not enough
#
# `rom/bench/e7_memfns`'s attribution against the frozen ROM identifies:
#   - boot-phase:            icount [0, 15,653,137)       -- WAD load, init
#   - store-heavy gameplay:  icount [233,932,753, 392,488,489)  -- frames
#     200->299 of real `-timedemo demo3` playback, the window
#     e7_memfns/README.md calls out as the one that matters (R_DrawColumn/
#     R_DrawSpan dominate -- both are pixel-store-bound rasterizers).
# Blending them into one whole-run average would hide exactly the effect
# `executor` found in #130: added correctness checks compound on
# memory-heavy code rather than diluting evenly across the instruction
# stream. Reporting the two windows separately is the point.
#
# Reaching icount 233,932,753 by live-executing the SQL CPU would cost tens
# of hours at ~1,000-2,000 instr/sec (ADR-0004) -- not payable once, let
# alone every sprint measurement. `gen_snapshot.py` runs the SAME ROM
# through refemu instead (~0.9M instr/sec, rom/bench/e7_memfns/README.md),
# reaching that icount in minutes, and `seed_snapshot.py` loads the dumped
# state directly into an isolated database's `ram`/`batch_commit`. See both
# scripts' own docstrings for what is and is not captured.
#
# ## What this does, and does not, reimplement
#
# - ROM-hash assertion, decoded/ram density: `scripts/preflight_milestone.sh`,
#   called against each window's isolated database once it's loaded --
#   never a second copy of those checks (team lead's explicit instruction).
# - `executor/fold.py`'s `select_only()` for fold-alone measurement,
#   `batch()` + `executor/commit.py`'s four flushes for end-to-end --
#   exactly as `scripts/run_milestone.sh` already established, never
#   hand-rolled flush SQL (#101's lesson, repeated by every script that
#   touches these two modules).
# - `text_start_widx`/`text_end_widx` passed to `fold.py` are RAM_BASE-
#   *relative*, not absolute manifest byte offsets / 4 -- #144/#146's bug,
#   fixed there and not repeated here (see the comment at TEXT_START_WIDX
#   below for the derivation).
#
# ## K, HWM, and why HWM is NOT raised to guarantee retired == K
#
# K = 60,000, issue #80's analytic optimum (~59,750, flat 50,000-80,000
# after correcting for the CSE bug in #86 -- see #80's final comment).
# HWM = 20,000, the SPEC/production default (`executor/config.py`), used
# unchanged rather than inflated to trivially guarantee no truncation:
# raising HWM would change the write-log scan cost this benchmark is
# supposed to measure under real conditions. If the store-heavy gameplay
# window's density is high enough to trip HWM before K retires at these
# settings, that is itself a real, sprint-relevant finding -- and per
# requirement #4, this script refuses to report a throughput figure
# computed on a truncated batch rather than silently averaging it in.
#
# ## Contention detection
#
# Checked once before starting and once between windows (point-in-time,
# not continuous background monitoring -- see check_contention() below for
# what that does and does not catch). Aborts rather than caveats: a
# throughput number taken while something else was loading the shared
# container is not a lower-confidence version of the real number, it is a
# different, meaningless number that happens to look plausible.
#
# Usage:
#   rom/bench/canonical_throughput/run.sh \
#     --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
#     [--k 60000] [--hwm 20000] [--batches 3] \
#     [--gameplay-target-icount 233932753] [--snapshot-dir /tmp/clickdoom-canonical-throughput] \
#     [--host localhost --port 9000 --user default --password ... --client '...']
set -euo pipefail
cd "$(dirname "$0")/../../.."   # repo root (rom/bench/canonical_throughput/ -> ../../..)

BIN=""
MANIFEST=""
K=60000
HWM=20000
BATCHES=3
# STALE as of #175 (R_DrawColumn/R_DrawSpan unroll, PINNED_HASH 9a6a47d0...):
# this was frame 200's icount against the pre-unroll ROM
# (eabb12ed...); the unroll shifts every frame's icount (fewer
# instructions to reach the same frame -- #175's own frame-hash
# equivalence gate confirms the *rendered content* doesn't move, only the
# instruction cost to reach it), so this no longer lands on frame 200
# precisely. Needs a fresh rom/bench/e7_memfns profile against the new
# ROM to re-derive exactly -- flagged, not blocking this PR, since this
# bench's gameplay window is "a representative store-heavy stretch," not
# an asserted/gated value the way #29's milestone icount is.
GAMEPLAY_TARGET_ICOUNT=233932753
SNAPSHOT_DIR="${TMPDIR:-/tmp}/clickdoom-canonical-throughput"
HOST="localhost"
PORT="9000"
CH_USER="default"
PASSWORD="${CLICKHOUSE_PASSWORD:-}"
CLIENT="clickhouse-client"
CONTAINER="${CLICKDOOM_CH_CONTAINER:-clickdoom-ch}"

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --manifest) MANIFEST="$2"; shift 2 ;;
    --k) K="$2"; shift 2 ;;
    --hwm) HWM="$2"; shift 2 ;;
    --batches) BATCHES="$2"; shift 2 ;;
    --gameplay-target-icount) GAMEPLAY_TARGET_ICOUNT="$2"; shift 2 ;;
    --snapshot-dir) SNAPSHOT_DIR="$2"; shift 2 ;;
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --client) CLIENT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

for req in BIN MANIFEST; do
  if [ -z "${!req}" ]; then
    echo "::error::--${req,,} is required" >&2
    exit 1
  fi
done

fail() { echo "::error::CANONICAL THROUGHPUT BENCH FAILED: $1" >&2; exit 1; }

now_s() { python3 -c 'import time; print(time.time())'; }

# shellcheck disable=SC2206  # deliberate word-split, matches
# preflight_milestone.sh's/run_milestone.sh's identical CH_CMD line.
CH_CMD=($CLIENT)
ch() {
  local db="$1"; shift
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$db")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

# --- contention detection -------------------------------------------------
#
# Two independent point-in-time checks, not continuous monitoring: this
# guards the *start* of a window's measurement against a competing process
# already loading the machine, which is the failure this project has
# actually hit twice in one day. It does NOT catch contention that begins
# mid-window -- a several-minute benchmark run has no cheap way to abort
# mid-batch without discarding partial work, and `arrayFold` doesn't
# yield control back to check anyway. Checking again between windows (see
# main() below) narrows that gap without pretending to close it.
check_contention() {
  local cpu_pct load1 ncpu busy_ratio
  cpu_pct=$(docker stats --no-stream --format '{{.CPUPerc}}' "$CONTAINER" 2>/dev/null | tr -d '%' | head -1)
  if [ -z "$cpu_pct" ]; then
    fail "couldn't read docker stats for $CONTAINER -- is it running? (just up)"
  fi
  # Integer compare only (bash has no float arithmetic) -- truncates
  # 24.9% to 24, which is the conservative direction (rounds toward
  # "not yet over threshold", not toward a false abort).
  cpu_pct_int="${cpu_pct%%.*}"
  if [ "$cpu_pct_int" -ge 25 ]; then
    fail "$CONTAINER is already at ${cpu_pct}% CPU before this benchmark started anything -- something else is loading the shared container (a teammate's run, a leftover process). Coordinate by name and retry once it's clear, per CLAUDE.md's coordination protocol."
  fi

  load1=$(uptime | sed -E 's/.*load average[s]?: *([0-9.]+).*/\1/')
  ncpu=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
  # busy_ratio = load1 * 100 / ncpu, integer, avoiding bash float math.
  busy_ratio=$(( $(printf '%.0f' "$(python3 -c "print($load1 * 100)")") / ncpu ))
  if [ "$busy_ratio" -ge 60 ]; then
    fail "host 1-minute load average is $load1 across $ncpu cores (${busy_ratio}% busy) -- something other than this benchmark is loading the machine. Retry once it's quiet."
  fi
  echo "# contention check: $CONTAINER at ${cpu_pct}% CPU, host load1=$load1 across $ncpu cores (${busy_ratio}%) -- OK" >&2
}

# --- ROM hash, asserted before anything else runs -------------------------
ROM_SHA=$(shasum -a 256 "$BIN" | awk '{print $1}')
PINNED=$(cat rom/PINNED_HASH)
if [ "$ROM_SHA" != "$PINNED" ]; then
  fail "$BIN sha256 ($ROM_SHA) != rom/PINNED_HASH ($PINNED) -- refusing to measure an unpinned ROM. Every sprint number needs to trace back to the same binary."
fi
echo "# rom: $ROM_SHA == PINNED_HASH -- OK" >&2

GIT_SHA=$(git rev-parse HEAD)

# text_start_widx/text_end_widx: RAM_BASE-*relative*, not the manifest's
# absolute byte offsets / 4 -- #144/#146's bug (fold.py's WA is relative,
# clamped to [0, RAM_WORDS-1]; passing an absolute value there makes the
# SELF_MODIFY window's bound comparison unconditionally false). Re-derived
# here rather than trusted from memory: load_addr is subtracted from both
# before they reach fold.py.
TEXT_START=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_start'])")
TEXT_END=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_end'])")
LOAD_ADDR=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['load_addr'])")
TEXT_START_WORD=$(( TEXT_START / 4 ))
TEXT_END_WORD=$(( TEXT_END / 4 ))
RAM_BASE_WORD=$(( LOAD_ADDR / 4 ))
TEXT_START_WIDX=$(( TEXT_START_WORD - RAM_BASE_WORD ))
TEXT_END_WIDX=$(( TEXT_END_WORD - RAM_BASE_WORD ))
DECN=$(( TEXT_END_WORD - TEXT_START_WORD ))
RAM_WORDS=6291456  # SPEC §2: 24 MiB / 4

HERE="rom/bench/canonical_throughput"

# --- generated-SQL shape (bytes + AST node count), not just wall-clock ---
#
# select_only()'s/batch()'s step-expression SQL depends only on
# K/TEXT_START_WIDX/TEXT_END_WIDX/DECN/RAM_WORDS/HWM -- never on pc0/regs0
# (those only substitute into the *initial accumulator*, not the lambda
# body) -- so the SQL is byte-for-byte identical across every batch in
# both windows. Computed once, not per-batch: repeating an EXPLAIN AST
# round-trip 12 times for a query that never changes would be pure
# overhead for no new information.
#
# Wall-clock alone isn't the only axis worth reporting: a cost driver that
# scales with generated-SQL size/node count (e.g. a subexpression
# referenced many times, not recomputed once) can show up here even when
# throughput hasn't moved yet, or vice versa -- same "don't collapse two
# different units into one number" reasoning ADR-0004 applies to fold-vs-
# e2e. Same technique as executor/bench/e1_cse/run.sh: `EXPLAIN AST`,
# `wc -l` for node count, byte length of the raw SQL text.
report_sql_shape() {
  local db="$1"
  local fold_sql e2e_sql fold_bytes e2e_bytes fold_nodes e2e_nodes
  fold_sql=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.select_only($K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM,
                        pc0=$LOAD_ADDR, db='$db'))
")
  e2e_sql=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.batch($K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM, db='$db'))
")
  fold_bytes=$(printf '%s' "$fold_sql" | wc -c | tr -d ' ')
  e2e_bytes=$(printf '%s' "$e2e_sql" | wc -c | tr -d ' ')
  fold_nodes=$({ echo "EXPLAIN AST"; printf '%s' "$fold_sql"; } | ch "$db" --multiquery | wc -l | tr -d ' ')
  e2e_nodes=$({ echo "EXPLAIN AST"; printf '%s' "$e2e_sql"; } | ch "$db" --multiquery | wc -l | tr -d ' ')
  printf 'fold_sql_bytes\t%s\n' "$fold_bytes" >&2
  printf 'fold_ast_nodes\t%s\n' "$fold_nodes" >&2
  printf 'e2e_sql_bytes\t%s\n' "$e2e_bytes" >&2
  printf 'e2e_ast_nodes\t%s\n' "$e2e_nodes" >&2
}

# --- one fold-alone batch, chained forward from the given pc/regs --------
#
# Chained (each batch's returned pc/regs feeds the next), not repeated
# identically N times: this measures the real cost of running *through*
# the window, matching how the e2e loop and a real run both work, not a
# synthetic repeat of one already-measured batch. select_only() never
# touches batch_commit, so this needs no seeding beyond ram/decoded.
#
# Prints: seconds<TAB>retired<TAB>halted<TAB>halt_reason<TAB>next_pc<TAB>next_regs_csv
run_fold_batch() {
  local db="$1" pc="$2" regs_literal="$3"
  local sql out t0 t1
  # regs0 is wrapped per-element as 'toUInt32(N)' strings, not passed as
  # plain ints: fold.py's select_only() (executor/fold.py:574) builds the
  # regs0 SQL literal as a bare `"[" + ",".join(str(x) for x in regs0) +
  # "]"` with no type annotation, so an explicit regs0 whose values are
  # all small (the SPEC §1 reset vector this window's first batch needs --
  # 31 zeros) gets inferred by ClickHouse as Array(UInt8), not
  # Array(UInt32), and arrayFold then rejects it: "Return type of lambda
  # function must be the same as the accumulator type" (found empirically
  # testing this exact call, not assumed). Passing regs0=None instead
  # dodges it via select_only()'s *other* branch
  # (`arrayResize(emptyArrayUInt32(), ...)`, which IS typed) but only
  # produces zeros, not an arbitrary snapshot's real register values --
  # not usable for the gameplay window. `str(x)` on a Python *string*
  # element is a no-op, so wrapping each value as the string "toUInt32(N)"
  # before it reaches select_only() rides the exact same join unmodified
  # and forces the correct element type regardless of magnitude, without
  # editing fold.py itself (executor's scope, not signed off for this
  # task, and a bug worth its own review) -- filed as a follow-up issue.
  sql=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
regs0 = [f'toUInt32({x})' for x in $regs_literal]
print(fold.select_only($K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM,
                        pc0=$pc, regs0=regs0, db='$db'))
")
  t0=$(now_s)
  out=$(echo "$sql" | ch "$db" --format TSVWithNames)
  t1=$(now_s)
  # select_only()'s column order (executor/fold.py, same as preflight's
  # gate 4 comment): pc, regs, wl_addr, wl_val, wl_icount, stopped, halted,
  # halt_reason, halt_pc, halt_extra, retired, console_bytes, keyq_pos,
  # frame_no, frame_committed -- read by name from the header row, not
  # assumed positionally (#140's exact lesson). Piped via stdin, not
  # embedded in a python -c string literal: $out is real query output and
  # could in principle contain characters that break a naive triple-quoted
  # embedding (e.g. a halt_reason string), so it goes through argv-free
  # stdin instead, same reasoning as every SQL-over-stdin call elsewhere in
  # this script.
  printf '%s' "$out" | python3 -c "
import sys
lines = sys.stdin.read().splitlines()
header = lines[0].split('\t')
row = lines[-1].split('\t')
d = dict(zip(header, row))
print(f\"{$t1 - $t0}\t{d['retired']}\t{d['halted']}\t{d['halt_reason']}\t{d['pc']}\t{d['regs']}\")
"
}

# --- one e2e batch (batch() + all four commit.py flushes), against the
# window's current batch_commit state -------------------------------------
#
# Prints: seconds<TAB>retired<TAB>halted<TAB>halt_reason
run_e2e_batch() {
  local db="$1"
  local prev_icount cpu_state_rows batch_sql t0 t1 icount halted halt_reason retired
  # cpu_state is only populated by a commit.py cpu_state flush -- before
  # this window's first e2e batch flushes, it's empty (not an error, a
  # real empty-table SELECT), so read the seed row from batch_commit
  # directly instead (the same source commit.py's own flush reads from).
  # Counted first rather than distinguished by "was $() empty" so a real
  # connection failure below still aborts loudly under `set -e` instead of
  # being folded into the same "empty" case.
  cpu_state_rows=$(ch "$db" --query "SELECT count() FROM cpu_state")
  if [ "$cpu_state_rows" -eq 0 ]; then
    prev_icount=$(ch "$db" --query "SELECT icount FROM batch_commit ORDER BY batch_id DESC LIMIT 1")
  else
    prev_icount=$(ch "$db" --query "SELECT icount FROM cpu_state ORDER BY batch_id DESC LIMIT 1")
  fi
  batch_sql=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.batch($K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM, db='$db'))
")
  t0=$(now_s)
  echo "$batch_sql" | ch "$db" --multiquery
  ch "$db" --multiquery <<< "$(python3 executor/commit.py ram --db "$db")"
  ch "$db" --multiquery <<< "$(python3 executor/commit.py console_out --db "$db")"
  ch "$db" --multiquery <<< "$(python3 executor/commit.py cpu_state --db "$db")"
  ch "$db" --multiquery <<< "$(python3 executor/commit.py retention --db "$db")"
  t1=$(now_s)
  read -r icount halted halt_reason <<< "$(ch "$db" --query \
    "SELECT icount, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1" | tr '\t' ' ')"
  retired=$(( icount - prev_icount ))
  echo "$(python3 -c "print($t1 - $t0)")	$retired	$halted	$halt_reason"
}

# --- run both modes for one window, N batches each ------------------------
run_window() {
  local label="$1" db="$2" pc0="$3" regs0_literal="$4"
  echo "" >&2
  echo "# === window: $label (database=$db) ===" >&2

  echo "# preflight (gates 1-4: decoded density, ram density, ROM hash, fold-path smoke test)" >&2
  # --trace omitted: not in preflight's required-arg list, and its only use
  # there is an informational provenance line, never dereferenced as a
  # file. Gate 4's own smoke test always runs from pc0=RAM_BASE (a known,
  # separate #146 issue, not this script's to fix) -- it validates the
  # generic fold path and this window's decoded/ram density, not this
  # window's specific seeded pc/regs; the actual measurement below is what
  # exercises the real seeded state.
  if ! ./scripts/preflight_milestone.sh --bin "$BIN" --manifest "$MANIFEST" --k "$K" --hwm "$HWM" \
      --database "$db" --host "$HOST" --port "$PORT" --user "$CH_USER" \
      --password "$PASSWORD" --client "$CLIENT" >&2; then
    fail "$label: preflight gate did not pass"
  fi

  echo "# fold-alone: $BATCHES batches of K=$K, chained" >&2
  local pc="$pc0" regs="$regs0_literal"
  local fold_total_s=0 fold_total_retired=0
  for i in $(seq 1 "$BATCHES"); do
    local line s retired halted halt_reason next_pc next_regs
    line=$(run_fold_batch "$db" "$pc" "$regs")
    IFS=$'\t' read -r s retired halted halt_reason next_pc next_regs <<< "$line"
    if [ "$halted" = "0" ] && [ "$retired" != "$K" ]; then
      fail "$label fold-alone batch $i: retired $retired, not K=$K, and did not halt -- a truncated batch measures different work than a full one (requirement #4). If this is the write-log HWM ($HWM) binding on this window's real store density, that is itself a sprint-relevant finding -- report it, don't paper over it by lowering K or raising HWM without saying so."
    fi
    echo "#   fold batch $i: ${s}s retired=$retired halted=$halted halt_reason=$halt_reason" >&2
    fold_total_s=$(python3 -c "print($fold_total_s + $s)")
    fold_total_retired=$(( fold_total_retired + retired ))
    pc="$next_pc"
    regs="$next_regs"
  done
  local fold_instr_sec
  fold_instr_sec=$(python3 -c "print(f'{$fold_total_retired / $fold_total_s:.1f}')")
  echo "# fold-alone total: retired=$fold_total_retired seconds=$fold_total_s instr/sec=$fold_instr_sec" >&2

  echo "# e2e: $BATCHES batches of K=$K, chained through commit.py" >&2
  local e2e_total_s=0 e2e_total_retired=0
  for i in $(seq 1 "$BATCHES"); do
    local line s retired halted halt_reason
    line=$(run_e2e_batch "$db")
    IFS=$'\t' read -r s retired halted halt_reason <<< "$line"
    if [ "$halted" = "0" ] && [ "$retired" != "$K" ]; then
      fail "$label e2e batch $i: retired $retired, not K=$K, and did not halt -- same reasoning as the fold-alone check above."
    fi
    echo "#   e2e batch $i: ${s}s retired=$retired halted=$halted halt_reason=$halt_reason" >&2
    e2e_total_s=$(python3 -c "print($e2e_total_s + $s)")
    e2e_total_retired=$(( e2e_total_retired + retired ))
  done
  local e2e_instr_sec
  e2e_instr_sec=$(python3 -c "print(f'{$e2e_total_retired / $e2e_total_s:.1f}')")
  echo "# e2e total: retired=$e2e_total_retired seconds=$e2e_total_s instr/sec=$e2e_instr_sec" >&2

  printf '%s\tfold\t%s\t%s\t%s\t%s\n' "$label" "$K" "$HWM" "$fold_total_retired" "$fold_instr_sec"
  printf '%s\te2e\t%s\t%s\t%s\t%s\n' "$label" "$K" "$HWM" "$e2e_total_retired" "$e2e_instr_sec"
}

main() {
  check_contention

  local decoded_rows
  echo "# --- setting up boot window (fresh reset state) ---" >&2
  BOOT_DB="canonical_throughput_boot_$$"
  ch default --query "DROP DATABASE IF EXISTS $BOOT_DB"
  sed "s/clickdoom/$BOOT_DB/g" sqlcpu/schema.sql | ch default --multiquery
  python3 sqlcpu/load_rom.py --bin "$BIN" --manifest "$MANIFEST" --host "$HOST" --port "$PORT" \
      --user "$CH_USER" --password "$PASSWORD" --database "$BOOT_DB" --client "$CLIENT" >&2
  sed "s/clickdoom/$BOOT_DB/g" sqlcpu/decode.sql | \
      ch "$BOOT_DB" --multiquery --param_text_start_word="$TEXT_START_WORD" --param_text_end_word="$TEXT_END_WORD"
  python3 executor/bootstrap.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
      --password "$PASSWORD" --database "$BOOT_DB" --client "$CLIENT" >&2

  echo "# --- generating/reusing gameplay-window snapshot (icount=$GAMEPLAY_TARGET_ICOUNT) ---" >&2
  (cd refemu && uv run python "../$HERE/gen_snapshot.py" \
      --rom "../$BIN" --manifest "../$MANIFEST" \
      --target-icount "$GAMEPLAY_TARGET_ICOUNT" --out-dir "$SNAPSHOT_DIR") >&2
  SNAPSHOT_FILE="$SNAPSHOT_DIR/snapshot.${ROM_SHA:0:12}.${GAMEPLAY_TARGET_ICOUNT}.pkl"
  [ -f "$SNAPSHOT_FILE" ] || fail "expected snapshot at $SNAPSHOT_FILE, not found after gen_snapshot.py ran"

  echo "# --- setting up gameplay window (seeded from snapshot) ---" >&2
  GAMEPLAY_DB="canonical_throughput_gameplay_$$"
  ch default --query "DROP DATABASE IF EXISTS $GAMEPLAY_DB"
  sed "s/clickdoom/$GAMEPLAY_DB/g" sqlcpu/schema.sql | ch default --multiquery
  python3 sqlcpu/load_rom.py --bin "$BIN" --manifest "$MANIFEST" --host "$HOST" --port "$PORT" \
      --user "$CH_USER" --password "$PASSWORD" --database "$GAMEPLAY_DB" --client "$CLIENT" >&2
  sed "s/clickdoom/$GAMEPLAY_DB/g" sqlcpu/decode.sql | \
      ch "$GAMEPLAY_DB" --multiquery --param_text_start_word="$TEXT_START_WORD" --param_text_end_word="$TEXT_END_WORD"
  # Overwrites the ram load_rom.py just did with the snapshot's actual
  # mid-run RAM contents -- load_rom.py above only ran so decode.sql (which
  # reads `ram`'s text region to build `decoded`) had something to decode;
  # `ram`'s data rows are replaced wholesale next.
  ch "$GAMEPLAY_DB" --query "TRUNCATE TABLE ram"
  python3 "$HERE/seed_snapshot.py" --snapshot "$SNAPSHOT_FILE" --host "$HOST" --port "$PORT" \
      --user "$CH_USER" --password "$PASSWORD" --database "$GAMEPLAY_DB" --client "$CLIENT" >&2

  local snapshot_pc snapshot_regs
  read -r snapshot_pc snapshot_regs <<< "$(python3 -c "
import pickle
d = pickle.load(open('$SNAPSHOT_FILE', 'rb'))
regs = d['regs'][1:32]
print(d['pc'], '[' + ','.join(str(r) for r in regs) + ']')
")"

  cleanup() {
    ch default --query "DROP DATABASE IF EXISTS $BOOT_DB" 2>/dev/null || true
    ch default --query "DROP DATABASE IF EXISTS $GAMEPLAY_DB" 2>/dev/null || true
  }
  trap cleanup EXIT

  decoded_rows=$(ch "$BOOT_DB" --query "SELECT count(DISTINCT word_addr) FROM decoded")

  echo "" >&2
  echo "# --- generated-SQL shape (identical across both windows/every batch;" >&2
  echo "# computed once against the boot database) ---" >&2
  report_sql_shape "$BOOT_DB"

  echo "" >&2
  printf 'window\tmode\tk\thwm\tretired\tinstr_per_sec\n'
  # 15393136, not the pre-#175 15653137 -- #175's unroll (PINNED_HASH
  # 9a6a47d0...) moved the boot window's own end icount along with every
  # other frame's, confirmed via a live refemu run, not carried over by
  # arithmetic.
  run_window "boot: [0, 15393136)" "$BOOT_DB" "$LOAD_ADDR" "[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]"

  echo "" >&2
  echo "# re-checking contention before the second window" >&2
  check_contention

  run_window "store-heavy gameplay: [233932753, 392488489)" "$GAMEPLAY_DB" "$snapshot_pc" "$snapshot_regs"

  echo "" >&2
  echo "# --- provenance -----------------------------------------------" >&2
  printf 'rom_sha256\t%s\n' "$ROM_SHA" >&2
  printf 'decoded_rows\t%s\n' "$decoded_rows" >&2
  printf 'K\t%s\n' "$K" >&2
  printf 'HWM\t%s\n' "$HWM" >&2
  printf 'batches_per_mode\t%s\n' "$BATCHES" >&2
  printf 'git_sha\t%s\n' "$GIT_SHA" >&2
  echo "# ---------------------------------------------------------------------" >&2
}

main
