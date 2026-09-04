# ClickDOOM NATIVE

The contract for native mode: DOOM's tic simulation and renderer, written as
ClickHouse SQL, run against the shareware `doom1.wad`. `SPEC.md` is the
contract for emulation mode and does not apply here; `PURITY.md` says which of
its rules apply to which mode.

Native mode is correct when it agrees with the real engine. The reference
emulator runs the real DOOM binary and reports the engine's game state at every
tic and its framebuffer at every frame. Native mode has to produce the same
state row for every tic and the same 64,000-byte frame for every frame.

## 1. Level data

The driver inserts the WAD as raw lumps: one row per lump with its index, name,
enclosing map marker and bytes. Everything derived from a lump is derived in
SQL: map geometry decoded from the fixed-width records, texture composition
from `PNAMES` and `TEXTURE1`, sprite frames, flats, colormaps, the blockmap
cell lists, the BSP ancestor paths, and the level's initial state.

## 2. Constant tables

The engine's own constant tables (`states`, `mobjinfo`, `sprnames`,
`weaponinfo`, `finesine`, `finetangent`, `tantoangle`, `rndtable`,
`gammatable`, `fuzzoffset`, `checkcoord`, the sound ids, the animation and
switch name lists) are program data. They are generated from the vendored
engine source under `rom/vendor/doomgeneric/` by
`native/src/bin/gen_tables.rs` into `native/tables/*.tsv`, and a test fails
if regeneration differs from what is committed. A table that cannot be
traced to the vendored source this way does not belong in the tree.

## 3. The state row

One row per tic in `native_state`, keyed by the tic number. The field list and
its order are `spec/src/native_state.rs`; the reference emulator's probe writes
the same fields. Every `fixed_t` is `Int32`, angles are `UInt32`, enums are
their C values. Mobjs and sector thinkers are parallel array columns indexed by
slot in thinker-list order. A thinker's identity is the value of a global
counter taken when it was added; pointers between thinkers hold that identity,
0 for none. The probe cannot read identities out of RAM and writes 0 for them,
so parity compares by slot and ignores the identity columns. A message the
player or the heads-up display holds is stored as the xxh64 of its text, which
is what the probe can compute from a C string.

## 4. The tic

One input row `(tic, source, keys, mouse_dx, mouse_dy)` produces the state row
for `tic` from the state row for `tic - 1`. `source` 0 takes the tic command
from the demo lump; `source` 1 builds it from the key bits in
`spec::native_state::key` and the mouse deltas, as `G_BuildTiccmd` does. The
tic runs `P_PlayerThink`, the thinker list in creation order with the same
random-number draws as the engine, `P_UpdateSpecials`, then the status bar,
heads-up display and menu tickers.

## 5. The frame

One input row `(frame, tic, melt passes)` produces the frame for `frame` from
the state row for `tic` and the previous frame. The melt's pass count per frame
is what the engine's clock gave it, read off the reference run and loaded as
data with its provenance; SQL turns the per-frame passes into the melt's
running step. The frame is 320×200 8bpp bytes,
row-major, plus the palette index chosen by the status bar, an RGB rendering of
the two, and the frame hash `xxHash64(framebuffer || palette)` defined by
`spec::fb_hash`. The framebuffer persists between frames, as it does in the
engine: pixels the renderer does not draw keep their previous value.

## 6. Resident statements

Each of the two components is one long-lived `INSERT INTO ... SELECT ... FROM
input(...)` over a chunked HTTP body, analysed once per session. The statement
text leads the body, terminated by a newline, because a URL parameter is
limited to about 64 KB and these statements are larger; any `WITH` clause sits
after `INSERT INTO ... ` and before `SELECT`. Settings travel as URL
parameters: `max_insert_block_size = 1`, `min_insert_block_size_rows = 1`,
`min_insert_block_size_bytes = 1`, `input_format_parallel_parsing = 0`,
`max_block_size = 1`, `max_threads = 1`, `max_insert_threads = 1`,
`async_insert = 0`, and `max_query_size` set to the statement's byte length
plus 64. The server reads that many bytes before it parses, so the first row
after the statement is padding, `tic = 0`, at least 128 bytes, and is filtered
out. A statement error surfaces on the response only after the body closes,
so the driver reads the response concurrently and treats an early response as
failure. A statement that has already failed keeps accepting rows and commits
none, so the driver detects death by rows that stop landing. The driver sends
the row for tic t+1 only after the row for tic t is readable. A statement that
ends is reopened, and the session resumes from the highest committed tic.

Static data enters a statement as scalar constants evaluated once. A constant
array is held as one value per element, and in a statement dozens of
subqueries deep each element costs kilobytes, so the pixel pools and every
other large constant are `String`s indexed with `substring`; a string constant
is one value whatever its length. A per-frame array captured inside a lambda is
copied once per element of the array being mapped, so per-pixel lookups go to
constants only and per-frame data is consumed element-wise.

A statement costs what it holds rather than what a tic asks of it. Every node
is evaluated for every row, both arms of an `if` included, and a lambda's body
is evaluated even where the array it maps over is empty. `arrayFold` is the
one exception: it runs its body once per element and not at all for none, so
a stage a tic has no work for is written as the body of a fold over what it
has to do. A lambda body that reads neither of its parameters is evaluated
outside the lambda whatever the fold does, so such a body has to lead back to
one of them.

Analysis is paid once per session, and its cost is not a function of the
statement's size. A call taking two or more arrays costs about 160 ms of
analysis on ClickHouse 26.7.5.10, whatever the call is and whatever its body
does. The price is per call site and not per array, so one call over many
arrays is free where many calls over two are not, and it is charged whether a
lambda is present or not: `arrayZip(a, b)` costs what
`arrayFilter((v, k) -> …, a, b)` costs. A call over one array costs nothing
measurable. `arrayConcat` is the exception at about 20 ms a site, so list
growth is written the plain way.

The 160 ms is the price at the statement's present size and is smaller in a
smaller one, so it sizes a change rather than the whole. The way to hold it
down is to read the arrays at an index instead of passing them: `arrayMap(k ->
f(a[k], b[k]), arrayEnumerate(a))` takes one array where `arrayMap((x, y) ->
f(x, y), a, b)` takes two, and a filter over a flag becomes the places that
survive, worked out once, and each column read through them. Every piece
added to a statement is measured with `QueryAnalysisMicroseconds` from
`system.query_log`.

A body that has to be one binding, such as a fold whose steps each read what
the step before wrote, is kept to what the dependency forces inside it. The
driver's budget for the first tic (`FIRST_TIC_TIMEOUT` in
`driver/src/cli/native/diff.rs`) is sized for this analysis on a CI runner,
which is about four times slower than a development machine.

Rows reach the statement one block each. Rows written into a statement from a
`VALUES` list share one block whatever `max_insert_block_size` says, so every
row of that block reads the state from before the block; tests that drive a
statement without the driver use `numbers()` or one insert per row.

## 7. Parity

The reference emulator's probe writes one row per frame in the shape of
section 3, with the frame index and the engine's `gametic`. A parity run loads
those rows and reports the first tic at which any field differs, with the
field, both values and the thinker involved, then the first frame whose hash
differs. Two hashes pin `demo3`: frame 220 is `aa27f0470c7c5f3a` and the final
frame is `d303721d8116e877`.

One field is partly outside the comparison. `T_VerticalDoor` never reads a
door's `topcountdown` while the door goes up, and the engine leaves it as the
zone allocator returned it until the door reaches the top and writes it. So
`s_count` for a door thinker whose direction is up is left out of the
comparison, and every other element of every other field is compared.

## 8. Determinism

No SQL path in native mode reads a clock, a random function or the host
environment. Pacing to 35 Hz happens in the driver and never changes a
computed value.
