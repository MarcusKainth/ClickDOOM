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
REFEMU="${REFEMU:-./target/release/refemu}"
BIN="${ROM_BIN:-rom/build/doom-rv32im.bin}"
MANIFEST="${ROM_MANIFEST:-rom/build/manifest.json}"
SNAPSHOT_FORMAT=$(python3 -c "import sys; sys.path.insert(0, 'scripts'); from refemu_snapshot import FORMAT_VERSION; print(FORMAT_VERSION)")
FIXTURE_CACHE="${TMPDIR:-/tmp}/clickdoom-frame-fixture/fixture.${TARGET_ICOUNT}.v${SNAPSHOT_FORMAT}.rsnap"
PPM_REAL_OUT="$(mktemp -t clickdoom-ppm-real.XXXXXX)"
PPM_SYNTH_OUT="$(mktemp -t clickdoom-ppm-synth.XXXXXX)"

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
cleanup() {
  ch default --query "DROP DATABASE IF EXISTS $TESTDB" 2>/dev/null || true
  rm -f "$PPM_REAL_OUT" "$PPM_SYNTH_OUT"
}
trap cleanup EXIT

echo "# --- setting up fixture database ($TESTDB) ---" >&2
ch default --query "DROP DATABASE IF EXISTS $TESTDB"
sed "s/{{DB}}/$TESTDB/g" driver/fixture_schema.sql | ch default --multiquery

# --- test 0: sparse framebuffer/palette injection (issue #220) ---------
#
# frame_readout_sql() used to read framebuffer/palette with a bare
# `groupArray(value) FROM (... FINAL ORDER BY word_addr)`, correct only
# because DG_DrawFrame happens to write every word before FRAME_COMMIT --
# a property of when the query runs, not something it enforces. This
# deliberately constructs a genuinely sparse table (a partially-written
# region, per #220's own description) and checks BOTH the historical bare
# read (OLD_READOUT_SQL below, a literal copy of the pre-#220 query --
# kept here so this regression stays caught even though render.py no
# longer contains it) and the current, fixed render.frame_readout_sql()
# (NEW) against the same seed data. Two independent cases -- sparse
# FRAMEBUFFER with a fully dense PALETTE, and vice versa -- so a fix that
# only covers one side is still caught by the case it didn't fix.
#
# Fully synthetic, deterministic data (gen_sparse_frame_fixture.py) --
# no ROM/refemu CPU stepping needed for this test, and the expected
# fb_hash comes from refemu.trace.fb_hash() directly (the same oracle
# function, not a reimplementation), applied to an independently-built
# zero-filled byte string in Python.
run_sparse_case() {
  local which="$1" written="$2" case_label="$3"
  echo "# --- test 0.$case_label: sparse $which injection (issue #220) ---" >&2

  ch "$TESTDB" --query "TRUNCATE TABLE framebuffer"
  ch "$TESTDB" --query "TRUNCATE TABLE palette"
  ch "$TESTDB" --query "TRUNCATE TABLE batch_commit"
  ch "$TESTDB" --query "TRUNCATE TABLE frames_out"

  local sparse_fixture
  sparse_fixture="$(mktemp -t "clickdoom-sparse-${which}.XXXXXX").json"
  REFEMU="$REFEMU" python3 driver/gen_sparse_frame_fixture.py \
      --which "$which" --written-words "$written" --out "$sparse_fixture" >&2
  EXPECTED_SPARSE_FBHASH=$(python3 -c "
import json
with open('$sparse_fixture', encoding='utf-8') as f:
    print(json.load(f)['expected_fbhash'])
")

  python3 driver/seed_sparse_fixture.py --fixture "$sparse_fixture" --host "$HOST" --port "$PORT" \
      --user "$CH_USER" --password "$PASSWORD" --database "$TESTDB" --client "$CLIENT" \
      --frame-no 1 --icount 1 >&2

  # OLD: the pre-#220 bare groupArray/FINAL read, reproduced verbatim as
  # a historical negative control (not imported from render.py -- this
  # exact shape no longer exists there on purpose). Byte conversion
  # (region_bytes_sql) is still cited from render.py, since that half of
  # the query was never wrong.
  OLD_READOUT_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
db = '$TESTDB'
old_fb_words = f'(SELECT groupArray(value) FROM (SELECT value FROM {db}.framebuffer FINAL ORDER BY word_addr))'
old_pal_words = f'(SELECT groupArray(value) FROM (SELECT value FROM {db}.palette FINAL ORDER BY word_addr))'
fb_bytes = render.region_bytes_sql(old_fb_words)
pal_bytes = render.region_bytes_sql(old_pal_words)
print(f'''INSERT INTO {db}.frames_out (frame_no, committed_icount, fb, palette)
SELECT frame_no, icount, {fb_bytes} AS fb, {pal_bytes} AS palette
FROM (
    SELECT frame_no, icount
    FROM {db}.batch_commit
    WHERE has_frame = 1
    ORDER BY batch_id DESC
    LIMIT 1
)''')
")
  echo "$OLD_READOUT_SQL" | ch "$TESTDB" --multiquery
  OLD_FBHASH=$(python3 -c "
import sys
sys.path.insert(0, 'driver'); sys.path.insert(0, 'sqlcpu')
import render
print(render.frame_readout_fb_hash_sql(db='$TESTDB'))
" | ch "$TESTDB" --multiquery)

  ch "$TESTDB" --query "TRUNCATE TABLE frames_out"

  # NEW: the current, fixed render.frame_readout_sql() -- same seed data.
  NEW_READOUT_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver'); sys.path.insert(0, 'sqlcpu')
import render
print(render.frame_readout_sql(db='$TESTDB'))
")
  echo "$NEW_READOUT_SQL" | ch "$TESTDB" --multiquery
  NEW_FBHASH=$(python3 -c "
import sys
sys.path.insert(0, 'driver'); sys.path.insert(0, 'sqlcpu')
import render
print(render.frame_readout_fb_hash_sql(db='$TESTDB'))
" | ch "$TESTDB" --multiquery)

  echo "  which=$which written_words=$written expected=$EXPECTED_SPARSE_FBHASH old=$OLD_FBHASH new=$NEW_FBHASH" >&2

  if [ "$OLD_FBHASH" = "$EXPECTED_SPARSE_FBHASH" ]; then
    fail "sparse-$which negative control did not fail: the historical bare-groupArray query produced the expected hash anyway ($OLD_FBHASH) -- this case does not actually exercise sparseness, fix the test"
  fi
  if [ "$NEW_FBHASH" != "$EXPECTED_SPARSE_FBHASH" ]; then
    fail "sparse-$which: fixed frame_readout_sql() produced fb_hash=$NEW_FBHASH, expected $EXPECTED_SPARSE_FBHASH -- the dense-read fix does not reconstruct a genuinely sparse $which region correctly"
  fi
  echo "  test 0.$case_label OK: old(sparse, buggy)=$OLD_FBHASH != expected; new(dense, fixed)=$NEW_FBHASH == expected" >&2

  rm -f "$sparse_fixture"
}

run_sparse_case fb  100 a   # sparse FRAMEBUFFER (100/16000 words written), dense PALETTE
run_sparse_case pal 50  b   # sparse PALETTE (50/192 words written), dense FRAMEBUFFER

# Reset fixture tables before the real-fixture tests below reuse them.
ch "$TESTDB" --query "TRUNCATE TABLE framebuffer"
ch "$TESTDB" --query "TRUNCATE TABLE palette"
ch "$TESTDB" --query "TRUNCATE TABLE batch_commit"
ch "$TESTDB" --query "TRUNCATE TABLE frames_out"

echo "# --- generating/reusing real refemu data at icount=$TARGET_ICOUNT ---" >&2
mkdir -p "$(dirname "$FIXTURE_CACHE")"
if [ ! -f "$FIXTURE_CACHE" ]; then
  # Stops at the first announced frame and checks it landed where the
  # milestone says, so a fixture built against a moved ROM fails here rather
  # than several comparisons later.
  "$REFEMU" run "$BIN" --manifest "$MANIFEST" --pinned-hash rom/PINNED_HASH \
      --stop-at "frame:0" --max-instructions "$TARGET_ICOUNT" \
      --expect-icount "$TARGET_ICOUNT" --expect-fbhash "$EXPECTED_FBHASH" \
      --dump-frame "$FIXTURE_CACHE" >&2
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

echo "# --- test 1b: ppm_render_sql() against the SAME real data, tied to fb_hash ---" >&2
# ppm_render_sql()'s output is RGB bytes, a different representation than
# fb_hash's own domain (raw indexed fb||palette, SPEC §7) -- there is no
# hash value the two share directly. What ties them together instead: this
# is the exact same frames_out row test 1 just proved has fb_hash
# fe5d82c0f42d45f1, so an independent Python re-derivation of the expected
# RGB bytes from that SAME fixture's raw fb/palette (not the SQL's own
# logic, mirrored) is a genuine second check on the SAME known-correct
# frame, not a tautology against the SQL under test.
PPM_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
print(render.ppm_render_sql(db='$TESTDB'))
")
T0=$(python3 -c 'import time; print(time.time())')
echo "$PPM_SQL" | ch "$TESTDB" --format TSVRaw > "$PPM_REAL_OUT"
T1=$(python3 -c 'import time; print(time.time())')
PPM_SECONDS=$(python3 -c "print(f'{$T1 - $T0:.3f}')")

python3 -c "
import struct, sys
sys.path.insert(0, 'scripts')
from refemu_snapshot import load

_header, sections = load('$FIXTURE_CACHE', need=('framebuffer', 'palette'))
fb, palette = sections['framebuffer'], sections['palette']
assert len(fb) == 64_000 and len(palette) == 768

expected = b'P6\n320 200\n255\n'
pal_rgb = [palette[i * 3:i * 3 + 3] for i in range(256)]
expected += b''.join(pal_rgb[idx] for idx in fb)

# clickhouse-client --format TSVRaw appends a trailing row-terminator
# newline that is not part of the query's own String result -- verified
# separately via SELECT length(ppm_render_sql()) matching the PPM's true
# byte count exactly, not assumed from this comparison alone.
with open('$PPM_REAL_OUT', 'rb') as f:
    actual = f.read()
if actual.endswith(b'\n') and not expected.endswith(b'\n'):
    actual = actual[:-1]

if actual != expected:
    sys.exit(f'ppm_render_sql() output ({len(actual)} bytes) did not match the independently-'
              f'derived expected PPM ({len(expected)} bytes) for the same real, fb_hash-verified frame')
print(f'{len(actual)}')
" > /tmp/ppm_real_bytecount.txt || fail "$(cat /tmp/ppm_real_bytecount.txt 2>/dev/null || echo 'ppm_render_sql() real-data check failed')"
PPM_REAL_BYTES=$(cat /tmp/ppm_real_bytecount.txt)
echo "  ppm_render_sql(): $PPM_REAL_BYTES bytes, byte-exact match against an independent Python" >&2
echo "    re-derivation from the same fb_hash-verified real fixture -- OK (${PPM_SECONDS}s)" >&2

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

echo "# --- test 3: ppm_render_sql() against the same hand-computed synthetic case ---" >&2
# Same frames_out row test 2 just seeded -- ansi_render_sql() is a pure
# SELECT, nothing to re-seed.
PPM_SYNTH_SQL=$(python3 -c "
import sys
sys.path.insert(0, 'driver')
sys.path.insert(0, 'sqlcpu')
import render
print(render.ppm_render_sql(db='$TESTDB', width=2, height=2))
")
T0=$(python3 -c 'import time; print(time.time())')
echo "$PPM_SYNTH_SQL" | ch "$TESTDB" --format TSVRaw > "$PPM_SYNTH_OUT"
T1=$(python3 -c 'import time; print(time.time())')
PPM_SYNTH_SECONDS=$(python3 -c "print(f'{$T1 - $T0:.3f}')")

python3 -c "
import sys
expected = b'P6\n2 2\n255\n' + bytes([255,0,0, 0,255,0, 0,0,255, 255,255,0])
with open('$PPM_SYNTH_OUT', 'rb') as f:
    actual = f.read()
if actual.endswith(b'\n') and not expected.endswith(b'\n'):
    actual = actual[:-1]
if actual != expected:
    sys.exit(f'ppm_render_sql() synthetic output did not byte-match the hand-computed expected PPM\nexpected: {expected!r}\nactual:   {actual!r}')
" || fail "ppm_render_sql() synthetic 2x2 case did not byte-match"
echo "  ppm_render_sql(): byte-exact match on synthetic 2x2 case -- OK (${PPM_SYNTH_SECONDS}s)" >&2

echo "" >&2
echo "# --- provenance -----------------------------------------------" >&2
printf 'expected_fbhash\t%s\n' "$EXPECTED_FBHASH" >&2
printf 'target_icount\t%s\n' "$TARGET_ICOUNT" >&2
printf 'frame_readout_seconds\t%s\n' "$READOUT_SECONDS" >&2
printf 'ppm_render_real_seconds\t%s\n' "$PPM_SECONDS" >&2
printf 'ppm_render_real_bytes\t%s\n' "$PPM_REAL_BYTES" >&2
printf 'ansi_render_seconds\t%s\n' "$ANSI_SECONDS" >&2
printf 'ppm_render_synth_seconds\t%s\n' "$PPM_SYNTH_SECONDS" >&2
echo "# ---------------------------------------------------------------------" >&2
echo "" >&2
echo "ALL RENDER TESTS PASSED" >&2
