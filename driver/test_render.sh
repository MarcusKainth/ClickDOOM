#!/usr/bin/env bash
# Validates driver/render.py (issue #29) two ways, matching the plan
# posted on the issue:
#
#   1. frame_readout_sql() against REAL refemu data at the milestone
#      icount (#110's target -- 15,393,136 as of #175's unroll, PINNED_HASH
#      9a6a47d0...; was 15,653,137 before it) -- reproduces fb_hash
#      fe5d82c0f42d45f1 (unchanged by #175 -- that's the whole point of its
#      frame-hash equivalence gate) or this fails loudly. Not eyeballed:
#      sqlcpu/checkpoint.py's fb_hash() computes the check, never
#      reimplemented.
#   2. ansi_render_sql() against a small hand-computed synthetic case --
#      exact byte match against an independently-computed expected escape
#      sequence, not "looks right in a terminal."
#
# Runs entirely against an isolated, throwaway database
# (driver_render_test_<pid>) -- never the shared `clickdoom` database.
#
# Usage:
#   driver/test_render.sh [--client 'clickhouse-client'] [--host ...] [--port ...]
set -euo pipefail
cd "$(dirname "$0")/.."

HOST="localhost"
PORT="9000"
CH_USER="default"
PASSWORD="${CLICKHOUSE_PASSWORD:-}"
CLIENT="clickhouse-client"
# #175: R_DrawColumn/R_DrawSpan unrolled (rom/PINNED_HASH 9a6a47d0...) --
# the milestone frame's icount moved (fewer instructions to reach the same
# frame), fb_hash did not (frame-hash equivalence gate, #175, confirmed all
# 300 frames of the representative window identical). Both re-derived
# directly from a live refemu run against the current PINNED_HASH, not
# carried over by arithmetic on the pre-unroll numbers -- see #175 for why
# that specific shortcut isn't trusted today.
TARGET_ICOUNT=15393136
EXPECTED_FBHASH="fe5d82c0f42d45f1"
FIXTURE_CACHE="${TMPDIR:-/tmp}/clickdoom-frame-fixture/fixture.${TARGET_ICOUNT}.pkl"

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --client) CLIENT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

fail() { echo "::error::RENDER TEST FAILED: $1" >&2; exit 1; }

# shellcheck disable=SC2206
CH_CMD=($CLIENT)
ch() {
  local db="$1"; shift
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$db")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

TESTDB="driver_render_test_$$"
cleanup() { ch default --query "DROP DATABASE IF EXISTS $TESTDB" 2>/dev/null || true; }
trap cleanup EXIT

echo "# --- setting up fixture database ($TESTDB) ---" >&2
ch default --query "DROP DATABASE IF EXISTS $TESTDB"
sed "s/{{DB}}/$TESTDB/g" driver/fixture_schema.sql | ch default --multiquery

echo "# --- generating/reusing real refemu data at icount=$TARGET_ICOUNT ---" >&2
mkdir -p "$(dirname "$FIXTURE_CACHE")"
if [ ! -f "$FIXTURE_CACHE" ]; then
  (cd refemu && uv run python ../driver/gen_frame_fixture.py \
      --target-icount "$TARGET_ICOUNT" --out "$FIXTURE_CACHE") >&2
else
  echo "# reusing cached fixture: $FIXTURE_CACHE" >&2
fi

echo "# --- seeding fixture tables ---" >&2
python3 driver/seed_frame_fixture.py --fixture "$FIXTURE_CACHE" --host "$HOST" --port "$PORT" \
    --user "$CH_USER" --password "$PASSWORD" --database "$TESTDB" --client "$CLIENT" >&2

echo "# --- test 1: frame_readout_sql() against real data ---" >&2
READOUT_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
print(render.frame_readout_sql(db='$TESTDB'))
")
T0=$(python3 -c 'import time; print(time.time())')
echo "$READOUT_SQL" | ch "$TESTDB" --multiquery
T1=$(python3 -c 'import time; print(time.time())')
READOUT_SECONDS=$(python3 -c "print(f'{$T1 - $T0:.3f}')")

FRAMES_OUT_ROWS=$(ch "$TESTDB" --query "SELECT count() FROM frames_out")
[ "$FRAMES_OUT_ROWS" = "1" ] || fail "expected exactly 1 frames_out row after readout, got $FRAMES_OUT_ROWS"

FB_HASH_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
print(render.frame_readout_fb_hash_sql(db='$TESTDB'))
")
ACTUAL_FBHASH=$(echo "$FB_HASH_SQL" | ch "$TESTDB" --multiquery)
if [ "$ACTUAL_FBHASH" != "$EXPECTED_FBHASH" ]; then
  fail "frame_readout_sql() reconstructed fb/palette whose fb_hash is $ACTUAL_FBHASH, expected $EXPECTED_FBHASH -- the readout query does not reproduce the SPEC §7 oracle"
fi
echo "  frame_readout_sql(): fb_hash=$ACTUAL_FBHASH == $EXPECTED_FBHASH -- OK (${READOUT_SECONDS}s)" >&2

echo "# --- test 2: ansi_render_sql() against a hand-computed synthetic case ---" >&2
ch "$TESTDB" --query "TRUNCATE TABLE frames_out"
# 2x2 image: top-left=red(idx0), top-right=green(idx1), bottom-left=blue(idx2), bottom-right=yellow(idx3).
# fb bytes: 00 01 02 03 (row-major, 2 pixels/row). palette: idx0..3 set, rest zero-padded to 768.
PAL_HEX="ff000000ff000000ffffff00"
PAL_HEX="${PAL_HEX}$(python3 -c "print('00' * (768 - 12))")"
ch "$TESTDB" --query "INSERT INTO frames_out (frame_no, committed_icount, fb, palette) VALUES (0, 1, unhex('00010203'), unhex('$PAL_HEX'))"

ANSI_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
print(render.ansi_render_sql(db='$TESTDB', width=2, height=2))
")
T0=$(python3 -c 'import time; print(time.time())')
ACTUAL_ANSI=$(echo "$ANSI_SQL" | ch "$TESTDB" --format TSVRaw)
T1=$(python3 -c 'import time; print(time.time())')
ANSI_SECONDS=$(python3 -c "print(f'{$T1 - $T0:.3f}')")

EXPECTED_ANSI=$(python3 -c "
esc = chr(27)
def cell(fg, bg):
    return f'{esc}[38;2;{fg[0]};{fg[1]};{fg[2]}m{esc}[48;2;{bg[0]};{bg[1]};{bg[2]}m▀'
row = cell((255,0,0),(0,0,255)) + cell((0,255,0),(255,255,0)) + f'{esc}[0m'
print(row, end='')
")
if [ "$ACTUAL_ANSI" != "$EXPECTED_ANSI" ]; then
  fail "ansi_render_sql() output did not byte-match the hand-computed expected escape sequence
  expected: $(printf '%s' "$EXPECTED_ANSI" | cat -v)
  actual:   $(printf '%s' "$ACTUAL_ANSI" | cat -v)"
fi
echo "  ansi_render_sql(): byte-exact match on synthetic 2x2 case -- OK (${ANSI_SECONDS}s)" >&2

echo "" >&2
echo "# --- provenance -----------------------------------------------" >&2
printf 'expected_fbhash\t%s\n' "$EXPECTED_FBHASH" >&2
printf 'target_icount\t%s\n' "$TARGET_ICOUNT" >&2
printf 'frame_readout_seconds\t%s\n' "$READOUT_SECONDS" >&2
printf 'ansi_render_seconds\t%s\n' "$ANSI_SECONDS" >&2
echo "# ---------------------------------------------------------------------" >&2
echo "" >&2
echo "ALL RENDER TESTS PASSED" >&2
