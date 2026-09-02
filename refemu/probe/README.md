# refemu/probe/

DOOM's game state, read out of the reference emulator's RAM at every frame
commit. The native-mode SQL simulation writes rows of the same shape from its
own tables, and the two are compared column by column.

`clickdoom_spec::native_state` owns the field list and its order. Both writers
read it from there, so neither can drift without the other failing to build.

`layout.tsv` is the struct layout the probe reads offsets from.
[`DEVELOPING.md`](../../DEVELOPING.md) says how it is produced and what gates
it.

## Running it

    refemu probe rom/build/doom-rv32im.elf \
        --manifest rom/build/manifest.json \
        --pinned-hash rom/PINNED_HASH \
        --layout refemu/probe/layout.tsv \
        --stop-at halt --out-dir refemu/reference_traces/demo3 --name probe

It takes the ELF, not the flat binary: the state is read by symbol name and
only the ELF carries the symbol table. `--pinned-hash` compares the flattened
segments against the pinned hash rather than the file's own, because the ELF
file is not byte-reproducible. The compiler writes the name of its temporary
object file into `.strtab` and that name is random. The flattened segments are
the ROM.

`make gen-probe-trace` runs the whole demo and writes the rows beside the demo3
checkpoint trace. Like that trace the `.tsv` is not committed, and its
companion `.json` is. `make gen-probe-fixture` writes the committed fixture
under `fixtures/`.

## The random-call log

`--rng-out PATH`, or `--rng-name STEM` beside `--out-dir`, writes a second file
logging every call to the engine's `P_Random` with the function that made it:

    gametic  call_index  caller  caller_offset  icount

`call_index` counts calls from zero within each tic, so grouping by `gametic`
gives the sequence of calls that tic made. `caller` is the function containing
the return address in `ra`, and `caller_offset` is how far into it the call
sits, which separates two call sites in one function.

A state divergence says which column moved. This says which action function
asked for the number that moved it. `make gen-probe-trace` writes it beside the
state rows.

## The row

    frame_index  gametic  fb_hash  <every field of clickdoom_spec::native_state>

One row per frame commit. `frame_index` counts announced frames from zero and
is not the number the program writes. Arrays are ClickHouse TSV array syntax,
`[a,b,c]`.

Under `-timedemo` two tics run before the first frame is displayed, and the
screen melt commits many frames within one tic. The metadata's
`first_gameplay_frame` is the frame after the last one that repeated the
previous frame's `gametic`.

## Identities

The SQL side names a thinker by a counter it holds. Nothing in RAM carries that
counter, so the probe writes 0 for `m_id`, `s_seq`, `next_seq`, `next_linkseq`
and `m_linkseq`. **The parity query drops those five columns and compares by
slot.**

A slot is a thinker's one-based position in the thinker-list walk for that
frame. Mobjs and sector thinkers are numbered separately, in list order. A
pointer between thinkers is written as the pointed-to thinker's slot, and 0
when it is null: `m_target`, `m_tracer`, `p_mo`, `p_attacker` and
`sec_soundtarget` hold mobj slots, and `sec_specialdata` holds a sector-thinker
slot.

A pointer into one of the engine's static arrays becomes an index, and −1 when
it is null: `m_state` and `psp_state` index `states`, `m_subsector` indexes
`subsectors`, `m_player` is the player number, and `btn_line` indexes `lines`.

`p_message` and `hu_message` are the xxh64 of a C string, with
`clickdoom_spec::XXH64_SEED`, and 0 when there is none. `p_message` hashes the
pointer the player carries, which the heads-up code clears once it has taken
it. `hu_message` hashes the line the message widget is showing.

A thinker whose function is −1 has been removed and is waiting to be unlinked.
The probe skips it, because the SQL side has already dropped it. A thinker with
no function is in stasis: it is still on the list and still has state, and
`s_active` is 0 for it.

## Sector thinkers in one set of columns

Doors, plats, floors, ceilings and the four light thinkers are different C
structs sharing one set of columns. `s_kind` says which, with the values in
`clickdoom_spec::native_state::sector_thinker_kind`. A column a kind does not
have is 0.

| Column | Door | Plat | Floor | Ceiling | Light flash | Strobe | Glow | Fire flicker |
|---|---|---|---|---|---|---|---|---|
| `s_type` | `type` | `type` | `type` | `type` | | | | |
| `s_direction` | `direction` | | `direction` | `direction` | | | `direction` | |
| `s_speed` | `speed` | `speed` | `speed` | `speed` | | | | |
| `s_dest` | `topheight` | `low` | `floordestheight` | `bottomheight` | | | | |
| `s_dest2` | | `high` | | `topheight` | | | | |
| `s_count` | `topcountdown` | `count` | | | `count` | `count` | | `count` |
| `s_wait` | `topwait` | `wait` | | | | | | |
| `s_status` | | `status` | | | | | | |
| `s_oldstatus` | | `oldstatus` | | `olddirection` | | | | |
| `s_crush` | | `crush` | `crush` | `crush` | | | | |
| `s_tag` | | `tag` | | `tag` | | | | |
| `s_texture` | | | `texture` | | | | | |
| `s_newspecial` | | | `newspecial` | | | | | |
| `s_minlight` | | | | | `minlight` | `minlight` | `minlight` | `minlight` |
| `s_maxlight` | | | | | `maxlight` | `maxlight` | `maxlight` | `maxlight` |
| `s_mintime` | | | | | `mintime` | `darktime` | | |
| `s_maxtime` | | | | | `maxtime` | `brighttime` | | |

`s_activeplat_slot` and `s_activeceil_slot` are one-based positions in the
engine's `activeplats` and `activeceilings` tables, 0 when the thinker is in
neither. They matter because those tables are what put a plat or a ceiling into
stasis and take it out again.

## What the probe does not read

`demo_end` is 1 once `demoplayback` has gone false. Every other column comes
straight out of a named global or a struct field.
