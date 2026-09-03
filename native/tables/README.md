# native/tables

The DOOM engine's own constant tables, one TSV per C array. First line is
the column names, one line per entry after that, tab separated.

Generated from the vendored engine source, never edited by hand:

    cargo run -p clickdoom-native --bin gen_tables -- \
        rom/vendor/doomgeneric/doomgeneric native/tables

`native/tests/tables.rs` regenerates every table into a temporary directory
and compares it byte for byte with what is committed here, so an edit that
the source does not produce fails the suite.

| File | C array | Source |
|---|---|---|
| `states.tsv` | `states` | `info.c` |
| `action_functions.tsv` | the action functions `states` names | `info.c` |
| `mobjinfo.tsv` | `mobjinfo` | `info.c` |
| `sprnames.tsv` | `sprnames` | `info.c` |
| `sfxenum.tsv` | `sfxenum_t` | `sounds.h` |
| `weaponinfo.tsv` | `weaponinfo` | `d_items.c` |
| `animdefs.tsv` | `animdefs` | `p_spec.c` |
| `switchlist.tsv` | `alphSwitchList` | `p_switch.c` |
| `checkcoord.tsv` | `checkcoord` | `r_bsp.c` |
| `finetangent.tsv` | `finetangent` | `tables.c` |
| `finesine.tsv` | `finesine` | `tables.c` |
| `tantoangle.tsv` | `tantoangle` | `tables.c` |
| `rndtable.tsv` | `rndtable` | `m_random.c` |
| `fuzzoffset.tsv` | `fuzzoffset` | `r_draw.c` |
| `opposite.tsv` | `opposite` | `p_enemy.c` |
| `diags.tsv` | `diags` | `p_enemy.c` |
| `xspeed.tsv` | `xspeed` | `p_enemy.c` |
| `yspeed.tsv` | `yspeed` | `p_enemy.c` |
| `gammatable.tsv` | `gammatable` | `tables.c` |
| `messages.tsv` | every string the header defines | `d_englsh.h` |

The `id` column is the C array index, so a value that names another table's
entry is that table's `id`. `sfxenum` is an enumerator list rather than an
array, and its `id` is the value the compiler gives each name, which is what
`mobjinfo`'s five sound fields hold. `gammatable` is two-dimensional and
keys on `(level, id)`. `messages` is not an array at all and keys on the
name the header defines each string under.

## What the columns hold

Values are the C values. A `fixed_t` is 16.16, so `mobjinfo.radius` of
`1048576` is 16 map units. An angle is a binary angle over the full 32-bit
range. A flag word is the `|` of the `mobjflag_t` enumerators already
evaluated.

`states.action` is a number rather than a function name, and
`action_functions.tsv` maps it back. Zero is `NULL`, the state that runs
nothing; the rest are the action functions `states` names, sorted by name
and numbered from one.

`animdefs` and `switchlist` keep the terminator rows the engine stops at:
`animdefs.istexture` is `-1` on the last row, and `switchlist.name1` is
empty.

`messages.text` is the string literal's own bytes. C and TSV escape a
newline, a tab and a backslash the same way, so the cell is the literal with
its quotes taken off.
