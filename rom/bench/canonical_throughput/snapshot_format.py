"""Single source of truth for the `gen_snapshot.py` / `seed_snapshot.py`
pickle format version (issue #251).

Bump `FORMAT_VERSION` whenever the pickled state dict's *shape* changes in
a way that would make an old snapshot silently wrong if seeded under the
new code's assumptions. #251 is exactly that case: a version-1 (implicit,
unversioned) snapshot has no `framebuffer`/`palette` keys, and seeding one
as though it were current would put an all-zero palette into a
frame-verification run with no error anywhere -- a clean, plausible, WRONG
`fb_hash`, which is the precise silent failure #251 exists to close off.

`gen_snapshot.py` stamps every snapshot it writes with `FORMAT_VERSION`,
both inside the pickled dict (`state["format_version"]`) and in the output
filename (`snapshot_path()`) -- the filename bump means a pre-#251 cached
file on disk can never collide with (and be silently reused for) a
new-format request; the in-dict field is the second, independent check
`seed_snapshot.py` makes so that renaming or copying a stale file to a
new-format-looking path still gets caught. `seed_snapshot.py` refuses to
seed a state dict whose `format_version` is missing or does not equal this
constant.

This is a tiny module with zero dependencies (not even on `refemu`) on
purpose: `seed_snapshot.py` is invoked as a plain `python3` script, outside
`refemu`'s `uv` environment (see its own docstring / `run.sh`), so the
version constant it needs to check against cannot live inside a module
that transitively imports `refemu.cpu` (which `gen_snapshot.py` does, and
which pulls in `refemu`'s `xxhash` dependency) -- that would force
`seed_snapshot.py` to acquire a dependency it has never needed just to read
one integer.

History:
  1 -- implicit/unversioned. `pc`/`regs`/`ram`/`icount`/`ram_base`/
       `rom_sha256` only. No `framebuffer`/`palette`. Every snapshot ever
       generated before #251 is this shape, with no `format_version` key
       at all -- that absence is itself what `seed_snapshot.py` checks for.
  2 -- adds `framebuffer` (bytes, `FRAMEBUFFER_SIZE`=64,000, SPEC §2) and
       `palette` (bytes, `PALETTE_SIZE`=768, SPEC §2), both already
       region-relative (byte 0 = the region's own base), matching
       `refemu.memory.Memory.framebuffer`/`.palette`'s own representation
       and `sqlcpu/schema.sql`'s `framebuffer`/`palette` table convention
       (`executor/commit.py:fbpal_flush_sql()`'s documented "no RAM_BASE-
       style rebasing" asymmetry versus `ram`).
"""

FORMAT_VERSION = 2
