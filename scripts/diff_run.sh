#!/usr/bin/env bash
# The real differential runner (#227) -- and the #230 fix in the same tool:
# this compares the WHOLE SPEC §7 trace, not just RAM_HASH_INTERVAL rows.
#
# ## History (read before "simplifying" the cadence back down)
#
# scripts/diff_run.sh did not exist from d09b9ba onward: that commit added
# the `hashFiles('scripts/diff_run.sh')` guard on ci.yml's
# `differential-smoke` job in the SAME commit that deleted the file the
# guard checks for. The guard worked exactly as designed and the job has
# reported `success` on every PR since without ever executing a real
# comparison -- no differential run of any size had ever happened in this
# repository (#227). Separately, run_milestone.sh's own checkpoint
# comparison only ever lands on RAM_HASH_INTERVAL boundaries (1 in 256 of
# the checkpoint rows both engines actually emit) -- #230.
#
# This script is written to make BOTH mistakes structurally hard to repeat:
# it clamps every sqlcpu batch to CHECKPOINT_INTERVAL so it cannot skip a
# row, and its comparison-count assertion (bottom of this file) is computed
# ARITHMETICALLY from the actual final icount and cross-checked against
# refemu's own trace file content, not trusted from the loop that fed it --
# a runner that silently compares nothing must fail loudly here, not read
# as identical to one that compared everything.
#
# ## Why every sqlcpu batch is clamped to <= CHECKPOINT_INTERVAL (4,096)
#
# executor/fold.py's arrayFold accumulator holds no intermediate register
# state -- only the state after the full K-step fold. The only way to read
# the CPU's exact register file at a given icount is to make the batch
# actually end there. So every batch here requests at most
# `next_boundary - current_icount` instructions, landing exactly on the
# next CHECKPOINT_INTERVAL multiple (or the run's target N, whichever is
# closer) -- never further. There is deliberately NO --k flag: a caller
# passing a larger K would silently reintroduce #230's exact bug (batches
# sail past 4,096 multiples, most rows never compared), so the stride is
# structural, not configurable.
#
# This is NOT free: run_milestone.sh's production batches use K=50,000-
# 60,000, amortizing ClickHouse's fixed per-batch setup cost over many more
# retired instructions. At CHECKPOINT_INTERVAL that fixed cost stops being
# a rounding error and dominates the batch (#180's fitted per-batch model:
# S=1,754ms setup + a=0.3696ms/step puts K=4,096 at roughly HALF the
# instructions/sec of K=60,000). diff_run.sh is a correctness tool, not a
# throughput one -- run_milestone.sh keeps its own RAM_HASH_INTERVAL-only
# cadence unchanged for production runs, where speed matters and this
# script's full-trace guarantee does not apply.
#
# ## What each cadence can and cannot detect (read SPEC §7, and issue #191)
#
# Every CHECKPOINT_INTERVAL (4,096) landing compares icount/pc/reghash --
# REGISTERS AND CONTROL FLOW ONLY. Every RAM_HASH_INTERVAL (1,048,576)
# landing -- 256x rarer -- additionally compares ramhash/fbhash. #191
# bisected register-file equality against refemu across ~250,000
# instructions of bit-for-bit agreement, all 31 registers, while a RAM word
# sat unwritten (wrong value) the entire time -- zero footprint in any
# register until an instruction finally loaded from that address. The
# 4,096 cadence would not have caught that bug 256x earlier; it would not
# have caught it AT ALL until the next RAM_HASH_INTERVAL landing. Do not
# describe this script, here or in review, as "catching divergence 256x
# earlier" without that qualifier -- it catches REGISTER/CONTROL-FLOW
# divergence 256x earlier. Memory and framebuffer divergence remain
# detectable only at the RAM_HASH_INTERVAL cadence, exactly as SPEC §7
# defines it.
#
# ## FRAME_COMMIT and off-lattice batch endings (#223)
#
# Since #223, a batch also ends early on a FRAME_COMMIT write (SPEC §6),
# which will not generally land on a CHECKPOINT_INTERVAL multiple. This is
# NOT a skipped comparison: the loop below only compares when the actual
# retired icount (re-read from cpu_state after every batch, never assumed
# from the requested K) lands exactly on a 4,096 multiple. An off-lattice
# stop just means the current iteration compares nothing; the NEXT
# iteration recomputes its target boundary from the new actual icount and
# converges onto the next real checkpoint on its own -- the same
# never-trust-the-requested-K pattern run_milestone.sh already uses for its
# own (coarser) cadence. Neither engine has a checkpoint row at an
# off-lattice icount, so there is nothing to compare there either way.
#
# ## Ephemeral database, both engines run fresh, every invocation
#
# Every run provisions a throwaway database (schema.sql, load_rom.py,
# decode.sql, executor/bootstrap.py -- the same four steps
# preflight_milestone.sh assumes already happened, done here explicitly)
# and drives refemu via `python -m refemu` for the SAME N, exactly as that
# module's own docstring describes it as being built for. Neither engine
# resumes from prior state -- always icount 0, so a diff run's result never
# depends on `clickdoom`'s shared production state.
#
# Usage:
#   scripts/diff_run.sh N [--bin PATH] [--manifest PATH] [--hwm HWM]
#       [--database NAME] [--keep-db]
#       [--host H] [--port P] [--user U] [--password PW] [--client CLIENT]
#
#   N                 instruction count to run both engines for
#   --bin             ROM binary (default: rom/build/doom-rv32im.bin)
#   --manifest        ROM manifest.json (default: rom/build/manifest.json)
#   --hwm             write-log high-water mark (default: 20000, same
#                     convention as run_milestone.sh/preflight_milestone.sh)
#   --database        ephemeral database name (default: clickdoom_diff_$$,
#                     dropped on exit)
#   --keep-db         don't drop the database on exit (for inspecting a
#                     caught divergence)
set -euo pipefail
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
  echo "::error::usage: diff_run.sh N [--bin PATH] [--manifest PATH] [--hwm HWM] [--database NAME] [--keep-db] [--host H] [--port P] [--user U] [--password PW] [--client CLIENT]" >&2
  exit 1
fi
N="$1"; shift
case "$N" in
  ''|*[!0-9]*) echo "::error::N must be a positive integer instruction count, got '$N'" >&2; exit 1 ;;
esac
[ "$N" -gt 0 ] || { echo "::error::N must be a positive integer instruction count, got '$N'" >&2; exit 1; }

BIN="rom/build/doom-rv32im.bin"
MANIFEST="rom/build/manifest.json"
HWM="20000"
DATABASE=""
KEEP_DB=0
HOST="localhost"
PORT="9000"
CH_USER="default"
PASSWORD="${CLICKHOUSE_PASSWORD:-}"
CLIENT=""   # empty means auto-detect below; --client overrides explicitly

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --manifest) MANIFEST="$2"; shift 2 ;;
    --hwm) HWM="$2"; shift 2 ;;
    --database) DATABASE="$2"; shift 2 ;;
    --keep-db) KEEP_DB=1; shift ;;
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --client) CLIENT="$2"; shift 2 ;;
    *) echo "::error::unknown argument: $1" >&2; exit 1 ;;
  esac
done
[ -n "$DATABASE" ] || DATABASE="clickdoom_diff_$$"

fail() { echo "::error::DIFFERENTIAL RUN FAILED: $1" >&2; exit 1; }

CHECKPOINT_INTERVAL=4096       # SPEC §7
RAM_HASH_INTERVAL=1048576      # SPEC §7 -- 256x CHECKPOINT_INTERVAL

if [ -n "$CLIENT" ]; then
  # shellcheck disable=SC2206  # deliberate word-split, same convention as
  # every other script here that accepts a multi-word --client.
  CH_CMD=($CLIENT)
else
  # No --client given: locate a native-protocol client, installing one if
  # the runner has none -- the EXACT fallback sqlcpu/run_tests.sh already
  # uses (that script's own comment: "CI's ubuntu-latest runner ships
  # neither clickhouse-client nor clickhouse"). diff_run.sh is invoked
  # directly as a CI step with no separate install step of its own
  # (differential-smoke's first real run caught exactly this -- CI has
  # `clickhouse-client` available in the jobs that install it explicitly
  # for their own steps, but each job is its own fresh runner, so a step in
  # one job does not help another), so it needs to be able to provision
  # itself the same way run_tests.sh does, not assume an environment step
  # already did it.
  if command -v clickhouse-client >/dev/null 2>&1; then
    CH_CMD=(clickhouse-client)
  elif command -v clickhouse >/dev/null 2>&1; then
    CH_CMD=(clickhouse client)
  else
    echo "# no local clickhouse client found; fetching the standalone binary..." >&2
    fetch_dir="$(mktemp -d)"
    curl -fsSL https://clickhouse.com/ -o "$fetch_dir/install.sh"
    (cd "$fetch_dir" && sh install.sh)
    CH_CMD=("$fetch_dir/clickhouse" client)
  fi
fi
# Re-derive the --client STRING sqlcpu/load_rom.py and executor/bootstrap.py
# each take as their own argument (they invoke the client as a fresh
# subprocess, not through this script's ch()/ch_default()) from the same
# resolved CH_CMD -- otherwise an auto-detected client (CLIENT left empty
# above) would leave those two calls passing --client "" downstream, which
# `args.client.split()` turns into an empty argv, silently making the
# FIRST real argument after it (e.g. "--host") the program name instead.
# Caught exactly this way: differential-smoke's second real run failed
# with `FileNotFoundError: [Errno 2] No such file or directory: '--host'`.
CLIENT="${CH_CMD[*]}"
# Refuse loudly HERE if that resolution came out empty or broken, rather
# than let it surface three calls later as the obscure traceback above --
# same posture as preflight_milestone.sh's gates: refuse to proceed, don't
# advise and continue anyway.
if [ "${#CH_CMD[@]}" -eq 0 ]; then
  fail "clickhouse client resolution produced an EMPTY command (--client was given as blank/whitespace-only, or auto-detection above found nothing usable) -- refusing to proceed. A downstream call would silently treat its own next flag (e.g. --host) as the program name instead of failing here, at the actual point of the problem."
fi
if ! "${CH_CMD[@]}" --version >/dev/null 2>&1; then
  fail "resolved clickhouse client command '${CH_CMD[*]}' does not run (\`${CH_CMD[*]} --version\` failed) -- refusing to proceed rather than let this surface later as an unrelated-looking error from load_rom.py/bootstrap.py/a query. Check --client, or that the fetched/detected binary is actually executable."
fi
ch() {
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$DATABASE")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}
# Connects against `default` (which always exists), not `$DATABASE` --
# needed for the DROP/CREATE bootstrap of the ephemeral database itself,
# and for tearing it down in cleanup(). `clickhouse-client --database X`
# resolves X at connection time; pointing it at a database that doesn't
# exist yet (or no longer does) fails the connection before the query ever
# runs. Same convention executor/bench/commit_mutation/setup_db.sh and
# scripts/preflight_milestone.sh already use for exactly this reason.
ch_default() {
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database default)
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

REFTRACE=""
REFSTDERR=""
# shellcheck disable=SC2317,SC2329
# Invoked indirectly via `trap cleanup EXIT` below -- SC2317 ("unreachable")
# is shellcheck pre-0.10's mistaken read of a trap-invoked function; SC2329
# ("never invoked") is the newer check's name for the identical false
# positive. CI runs 0.9.0 (Ubuntu noble's apt package); local shellcheck is
# commonly newer -- disable both so this doesn't flip depending on which
# version happens to be checking it.
cleanup() {
  if [ "$KEEP_DB" -eq 0 ]; then
    ch_default --query "DROP DATABASE IF EXISTS $DATABASE" 2>/dev/null || true
  else
    echo "# --keep-db: leaving database '$DATABASE' in place for inspection" >&2
  fi
  [ -n "$REFTRACE" ] && rm -f "$REFTRACE"
  [ -n "$REFSTDERR" ] && rm -f "$REFSTDERR"
}
trap cleanup EXIT

# Reports one caught divergence in the exact field order
# .github/ISSUE_TEMPLATE/divergence-report.yml asks for, so its output can
# be pasted straight into that form. $1=actual TSV line (sqlcpu),
# $2=expected TSV line (refemu), $3=icount.
report_checkpoint_divergence() {
  echo "ROM sha256: $ROM_SHA" >&2
  echo "ClickHouse version: $CH_VERSION" >&2
  echo "First divergent instruction (icount): $3" >&2
  echo "PC at divergence (hex, from refemu): 0x$(echo "$2" | cut -f2)" >&2
  echo "State diff:" >&2
  ACTUAL_LINE="$1" EXPECTED_LINE="$2" python3 - <<'PYEOF' >&2
import os
names = ["icount", "pc_hex", "reghash_hex", "ramhash_hex", "fbhash_hex"]
actual = os.environ["ACTUAL_LINE"].split("\t")
expected = os.environ["EXPECTED_LINE"].split("\t")
diffs = []
for i, name in enumerate(names[:len(expected)]):
    a = actual[i] if i < len(actual) else "<missing>"
    e = expected[i]
    if a != e:
        diffs.append(f"  {name}: expected(refemu)={e} actual(sqlcpu)={a}")
print("\n".join(diffs) if diffs else "  (lines differ but no per-field mismatch found -- check field count/whitespace)")
PYEOF
  echo "One-line repro: just diff $N" >&2
}

echo "# --- gate: ROM matches rom/PINNED_HASH ---------------------------------" >&2
PINNED=$(cat rom/PINNED_HASH)
if command -v sha256sum >/dev/null 2>&1; then
  ROM_SHA=$(sha256sum "$BIN" | cut -d' ' -f1)
else
  ROM_SHA=$(shasum -a 256 "$BIN" | cut -d' ' -f1)
fi
[ "$ROM_SHA" = "$PINNED" ] || fail "$BIN sha256 ($ROM_SHA) != rom/PINNED_HASH ($PINNED) -- a diff run against the wrong binary produces a meaningless divergence report. Rebuild (just build-rom) or check out the right commit."
echo "  rom: $ROM_SHA == PINNED_HASH -- OK" >&2

echo "# --- provisioning ephemeral database '$DATABASE' (fresh, icount 0) ----" >&2
TEXT_START=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_start'])")
TEXT_END=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['text_end'])")
LOAD_ADDR=$(python3 -c "import json; print(json.load(open('$MANIFEST'))['load_addr'])")
TEXT_START_WORD=$(( TEXT_START / 4 ))
TEXT_END_WORD=$(( TEXT_END / 4 ))
DECN=$(( TEXT_END_WORD - TEXT_START_WORD ))
RAM_BASE_WORD=$(( LOAD_ADDR / 4 ))
TEXT_START_WIDX=$(( TEXT_START_WORD - RAM_BASE_WORD ))
TEXT_END_WIDX=$(( TEXT_END_WORD - RAM_BASE_WORD ))
RAM_WORDS=6291456  # SPEC §2: 24 MiB / 4, same constant every other script uses

ch_default --query "DROP DATABASE IF EXISTS $DATABASE"
ch_default --query "CREATE DATABASE $DATABASE"
sed "s/clickdoom/${DATABASE}/g" sqlcpu/schema.sql | ch --multiquery
python3 sqlcpu/load_rom.py --bin "$BIN" --manifest "$MANIFEST" --host "$HOST" --port "$PORT" \
    --user "$CH_USER" --password "$PASSWORD" --database "$DATABASE" --client "$CLIENT" >&2
sed "s/clickdoom/${DATABASE}/g" sqlcpu/decode.sql | \
    ch --multiquery --param_text_start_word="$TEXT_START_WORD" --param_text_end_word="$TEXT_END_WORD"
python3 executor/bootstrap.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
    --password "$PASSWORD" --database "$DATABASE" --client "$CLIENT" >&2
CH_VERSION=$(ch --query "SELECT version()")
echo "  provisioned: decoded rows=$DECN, ClickHouse $CH_VERSION" >&2

echo "# --- generating refemu's SPEC §7 trace for N=$N instructions -----------" >&2
REFTRACE=$(mktemp)
REFSTDERR=$(mktemp)
REFEMU_STATUS=0
( cd refemu && uv run python -m refemu "../$BIN" --manifest "../$MANIFEST" --max-instructions "$N" ) \
    >"$REFTRACE" 2>"$REFSTDERR" || REFEMU_STATUS=$?
REFEMU_LINES=$(wc -l < "$REFTRACE" | tr -d ' ')
echo "  refemu: $REFEMU_LINES trace lines, exit=$REFEMU_STATUS" >&2

REFEMU_HALTED=0
REFEMU_HALT_REASON=""
REFEMU_HALT_PC="0"
REFEMU_HALT_ICOUNT="-1"
REFEMU_HALT_LINE=$(grep -m1 '^# halted:' "$REFSTDERR" || true)
if [ -n "$REFEMU_HALT_LINE" ]; then
  REFEMU_HALTED=1
  read -r REFEMU_HALT_REASON REFEMU_HALT_PC REFEMU_HALT_ICOUNT <<< "$(python3 -c "
import re
m = re.search(r'# halted: (\S+) at pc=0x([0-9a-fA-F]+) icount=(\d+)', open('$REFSTDERR').read())
reason, pc, icount = (m.group(1), int(m.group(2), 16), m.group(3)) if m else ('UNKNOWN', 0, -1)
print(reason, pc, icount)
")"
  echo "  refemu: $REFEMU_HALT_LINE" >&2
fi

echo "# --- differential loop: sqlcpu batches clamped to CHECKPOINT_INTERVAL --" >&2
ICOUNT=0
CHECKPOINTS_COMPARED=0
RAM_HASH_CHECKPOINTS_COMPARED=0
SQLCPU_HALTED=0
SQLCPU_HALT_REASON=""
SQLCPU_HALT_PC="0"
BATCHES_RUN=0
T0=$(date +%s)
# Wall clock here is a REPORTING instrument only (elapsed time, for the
# throughput line at the end) -- never fed into any computation the CPU or
# game logic depends on (SPEC §8.1 forbids exactly that). Same category as
# every other script here that times itself with `date`.

while [ "$ICOUNT" -lt "$N" ] && [ "$SQLCPU_HALTED" -eq 0 ]; do
  NEXT_BOUNDARY=$(( ((ICOUNT / CHECKPOINT_INTERVAL) + 1) * CHECKPOINT_INTERVAL ))
  STOP_AT=$(( NEXT_BOUNDARY < N ? NEXT_BOUNDARY : N ))
  STEP_K=$(( STOP_AT - ICOUNT ))
  [ "$STEP_K" -gt 0 ] || break

  BATCH_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'executor')
import fold
print(fold.batch($STEP_K, $TEXT_START_WIDX, $TEXT_END_WIDX, $DECN, $RAM_WORDS, $HWM, db='$DATABASE'))
")
  echo "$BATCH_SQL" | ch --multiquery
  BATCHES_RUN=$(( BATCHES_RUN + 1 ))

  # Checked assignments, not `ch --multiquery <<< "$(...)"` directly:
  # #228's finding -- a command substitution feeding a here-string discards
  # its own exit status even under `set -euo pipefail`, so a raising
  # commit.py call would hand `ch` empty stdin (accepted, exit 0) instead
  # of failing the run. Same pattern run_milestone.sh already uses.
  RAM_SQL=$(python3 executor/commit.py ram --db "$DATABASE")
  ch --multiquery <<< "$RAM_SQL"
  FBPAL_SQL=$(python3 executor/commit.py fbpal --db "$DATABASE")
  ch --multiquery <<< "$FBPAL_SQL"
  CONSOLE_OUT_SQL=$(python3 executor/commit.py console_out --db "$DATABASE")
  ch --multiquery <<< "$CONSOLE_OUT_SQL"
  CPU_STATE_SQL=$(python3 executor/commit.py cpu_state --db "$DATABASE")
  ch --multiquery <<< "$CPU_STATE_SQL"
  RETENTION_SQL=$(python3 executor/commit.py retention --db "$DATABASE")
  ch --multiquery <<< "$RETENTION_SQL"

  # Never trust STEP_K as what actually retired -- a batch can stop early
  # on halt, FRAME_COMMIT, or the write-log high-water mark. Re-read the
  # real state every iteration, same as run_milestone.sh.
  read -r ICOUNT PC HALTED HALT_REASON <<< "$(ch --query \
      "SELECT icount, pc, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1" | tr '\t' ' ')"

  if [ "$ICOUNT" -gt 0 ] && [ "$((ICOUNT % CHECKPOINT_INTERVAL))" -eq 0 ]; then
    if [ "$REFEMU_HALTED" -eq 1 ] && [ "$ICOUNT" -gt "$REFEMU_HALT_ICOUNT" ]; then
      fail "sqlcpu retired past icount=$ICOUNT but refemu halted earlier, at icount=$REFEMU_HALT_ICOUNT (reason=$REFEMU_HALT_REASON) -- refemu(expected) stopped; sqlcpu(actual) kept running.
  ROM sha256: $ROM_SHA
  ClickHouse version: $CH_VERSION
  First divergent instruction (icount): $REFEMU_HALT_ICOUNT
  PC at divergence (hex, from refemu): 0x$(printf '%08x' "$REFEMU_HALT_PC")
  State diff: refemu halted ($REFEMU_HALT_REASON) here; sqlcpu had no corresponding halt and continued to icount=$ICOUNT
  One-line repro: just diff $N"
    fi

    AT_RAM_HASH=0
    [ "$((ICOUNT % RAM_HASH_INTERVAL))" -eq 0 ] && AT_RAM_HASH=1

    if [ "$AT_RAM_HASH" -eq 1 ]; then
      ACTUAL_LINE=$(ch --format TSVRaw <<< "$(python3 scripts/checkpoint_query.py --db "$DATABASE")")
    else
      ACTUAL_LINE=$(ch --format TSVRaw <<< "$(python3 scripts/checkpoint_query.py --db "$DATABASE" --reg-only)")
    fi

    EXPECTED_LINE=$(awk -F'\t' -v ic="$ICOUNT" '$1 == ic { print; found=1 } END { if (!found) exit 1 }' "$REFTRACE") \
      || fail "no refemu trace line for icount=$ICOUNT -- refemu and sqlcpu disagree about where a checkpoint falls, which should be structurally impossible (both use CHECKPOINT_INTERVAL=$CHECKPOINT_INTERVAL)"

    if [ "$ACTUAL_LINE" != "$EXPECTED_LINE" ]; then
      echo "::error::checkpoint mismatch at icount=$ICOUNT" >&2
      report_checkpoint_divergence "$ACTUAL_LINE" "$EXPECTED_LINE" "$ICOUNT"
      fail "checkpoint mismatch at icount=$ICOUNT (divergence-report fields printed above)"
    fi

    CHECKPOINTS_COMPARED=$(( CHECKPOINTS_COMPARED + 1 ))
    [ "$AT_RAM_HASH" -eq 1 ] && RAM_HASH_CHECKPOINTS_COMPARED=$(( RAM_HASH_CHECKPOINTS_COMPARED + 1 ))
  fi

  if [ "$HALTED" = "1" ]; then
    SQLCPU_HALTED=1
    SQLCPU_HALT_REASON="$HALT_REASON"
    SQLCPU_HALT_PC="$PC"
  fi
done
T1=$(date +%s)

echo "# --- halt-shape comparison ----------------------------------------------" >&2
if [ "$SQLCPU_HALTED" -eq 1 ] && [ "$REFEMU_HALTED" -eq 0 ]; then
  fail "sqlcpu halted (reason=$SQLCPU_HALT_REASON icount=$ICOUNT pc=0x$(printf '%08x' "$SQLCPU_HALT_PC")) but refemu did not halt in [0,$N).
  ROM sha256: $ROM_SHA
  ClickHouse version: $CH_VERSION
  First divergent instruction (icount): $ICOUNT
  PC at divergence (hex, from refemu): <refemu never halted, no reference pc>
  State diff: sqlcpu halted ($SQLCPU_HALT_REASON) here; refemu had no corresponding halt
  One-line repro: just diff $N"
elif [ "$SQLCPU_HALTED" -eq 0 ] && [ "$REFEMU_HALTED" -eq 1 ] && [ "$ICOUNT" -ge "$REFEMU_HALT_ICOUNT" ]; then
  fail "refemu halted (reason=$REFEMU_HALT_REASON icount=$REFEMU_HALT_ICOUNT pc=0x$(printf '%08x' "$REFEMU_HALT_PC")) but sqlcpu did not.
  ROM sha256: $ROM_SHA
  ClickHouse version: $CH_VERSION
  First divergent instruction (icount): $REFEMU_HALT_ICOUNT
  PC at divergence (hex, from refemu): 0x$(printf '%08x' "$REFEMU_HALT_PC")
  State diff: refemu halted ($REFEMU_HALT_REASON) here; sqlcpu had no corresponding halt
  One-line repro: just diff $N"
elif [ "$SQLCPU_HALTED" -eq 1 ] && [ "$REFEMU_HALTED" -eq 1 ]; then
  if [ "$ICOUNT" != "$REFEMU_HALT_ICOUNT" ] || [ "$SQLCPU_HALT_REASON" != "$REFEMU_HALT_REASON" ]; then
    fail "halt shape mismatch: refemu(reason=$REFEMU_HALT_REASON icount=$REFEMU_HALT_ICOUNT pc=0x$(printf '%08x' "$REFEMU_HALT_PC")) vs sqlcpu(reason=$SQLCPU_HALT_REASON icount=$ICOUNT pc=0x$(printf '%08x' "$SQLCPU_HALT_PC")).
  ROM sha256: $ROM_SHA
  ClickHouse version: $CH_VERSION
  First divergent instruction (icount): $REFEMU_HALT_ICOUNT
  PC at divergence (hex, from refemu): 0x$(printf '%08x' "$REFEMU_HALT_PC")
  State diff: halt_reason expected(refemu)=$REFEMU_HALT_REASON actual(sqlcpu)=$SQLCPU_HALT_REASON; icount expected=$REFEMU_HALT_ICOUNT actual=$ICOUNT
  One-line repro: just diff $N"
  fi
  echo "  both engines halted identically: reason=$SQLCPU_HALT_REASON icount=$ICOUNT -- not a divergence" >&2
else
  echo "  neither engine halted in [0,$N) -- nothing to compare here" >&2
fi

echo "# --- comparison-count assertion (the non-negotiable) ---------------------" >&2
FINAL_ICOUNT="$ICOUNT"
EXPECTED_CHECKPOINTS=$(( FINAL_ICOUNT / CHECKPOINT_INTERVAL ))
EXPECTED_RAM_HASH_CHECKPOINTS=$(( FINAL_ICOUNT / RAM_HASH_INTERVAL ))
# Cross-checked against what refemu's OWN trace file actually contains up
# to FINAL_ICOUNT -- not only an arithmetic derivation trusting itself.
# A silent shortfall on either side is exactly #227's failure shape
# recurring in a new spot.
REFEMU_ROWS_IN_RANGE=$(awk -F'\t' -v ic="$FINAL_ICOUNT" '$1 <= ic { n++ } END { print n+0 }' "$REFTRACE")
REFEMU_RAM_HASH_ROWS_IN_RANGE=$(awk -F'\t' -v ic="$FINAL_ICOUNT" 'NF==5 && $1 <= ic { n++ } END { print n+0 }' "$REFTRACE")

echo "  checkpoints_compared=$CHECKPOINTS_COMPARED expected=$EXPECTED_CHECKPOINTS (refemu trace has $REFEMU_ROWS_IN_RANGE rows <= icount=$FINAL_ICOUNT)" >&2
echo "  ram_hash_checkpoints_compared=$RAM_HASH_CHECKPOINTS_COMPARED expected=$EXPECTED_RAM_HASH_CHECKPOINTS (refemu trace has $REFEMU_RAM_HASH_ROWS_IN_RANGE 5-field rows <= icount=$FINAL_ICOUNT)" >&2

[ "$REFEMU_ROWS_IN_RANGE" -eq "$EXPECTED_CHECKPOINTS" ] || \
  fail "refemu's own trace has $REFEMU_ROWS_IN_RANGE rows <= icount=$FINAL_ICOUNT, expected $EXPECTED_CHECKPOINTS by SPEC §7's own cadence arithmetic -- the oracle disagrees with the spec it's supposed to define; investigate refemu before trusting anything else this run reported"
[ "$CHECKPOINTS_COMPARED" -eq "$EXPECTED_CHECKPOINTS" ] || \
  fail "compared $CHECKPOINTS_COMPARED of $EXPECTED_CHECKPOINTS expected CHECKPOINT_INTERVAL rows in [0,$FINAL_ICOUNT] -- this IS #227's own failure shape: a runner that silently skips rows is indistinguishable from one that checked them all unless this assertion fires"
[ "$REFEMU_RAM_HASH_ROWS_IN_RANGE" -eq "$EXPECTED_RAM_HASH_CHECKPOINTS" ] || \
  fail "refemu's own trace has $REFEMU_RAM_HASH_ROWS_IN_RANGE 5-field rows <= icount=$FINAL_ICOUNT, expected $EXPECTED_RAM_HASH_CHECKPOINTS"
[ "$RAM_HASH_CHECKPOINTS_COMPARED" -eq "$EXPECTED_RAM_HASH_CHECKPOINTS" ] || \
  fail "compared $RAM_HASH_CHECKPOINTS_COMPARED of $EXPECTED_RAM_HASH_CHECKPOINTS expected RAM_HASH_INTERVAL rows in [0,$FINAL_ICOUNT] -- memory/framebuffer divergence (#191) is ONLY ever checked at this cadence; a shortfall here is silently blind to the likeliest divergence class in this system"

ELAPSED=$(( T1 - T0 ))
INSTR_PER_SEC=0
[ "$ELAPSED" -gt 0 ] && INSTR_PER_SEC=$(( FINAL_ICOUNT / ELAPSED ))

echo "" >&2
echo "# --- result ---------------------------------------------------------" >&2
printf 'rom_sha256\t%s\n' "$ROM_SHA"
printf 'clickhouse_version\t%s\n' "$CH_VERSION"
printf 'requested_instructions\t%s\n' "$N"
printf 'final_icount\t%s\n' "$FINAL_ICOUNT"
printf 'batches_run\t%s\n' "$BATCHES_RUN"
printf 'checkpoints_compared\t%s\n' "$CHECKPOINTS_COMPARED"
printf 'checkpoints_expected\t%s\n' "$EXPECTED_CHECKPOINTS"
printf 'ram_hash_checkpoints_compared\t%s\n' "$RAM_HASH_CHECKPOINTS_COMPARED"
printf 'ram_hash_checkpoints_expected\t%s\n' "$EXPECTED_RAM_HASH_CHECKPOINTS"
printf 'sqlcpu_halted\t%s\n' "$SQLCPU_HALTED"
printf 'elapsed_seconds\t%s\n' "$ELAPSED"
printf 'instructions_per_second\t%s\n' "$INSTR_PER_SEC"
echo "# -----------------------------------------------------------------------" >&2
echo "diff_run.sh: no divergence found -- $CHECKPOINTS_COMPARED register checkpoints compared through icount=$FINAL_ICOUNT ($RAM_HASH_CHECKPOINTS_COMPARED of them also memory+framebuffer checkpoints). Register/control-flow divergence would have been caught at the finer cadence; memory divergence is only ever checked at the RAM_HASH_INTERVAL points (#191)." >&2
exit 0
