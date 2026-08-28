"""Write-log seed shapes for issue #257.

The question is whether per-step cost inside `arrayFold` grows with the length
of RAM's write-log (`acc.3`). Nothing in normal operation can vary that length
independently of K -- the log only grows by retiring stores -- so this module
builds initial accumulators whose write-log starts non-empty, and
`fold.select_only(..., wl0=...)` consumes them.

## The two costs that scale with write-log length

There are TWO, not one, and this matters for what can be concluded:

  1. the load-forwarding scan. `arrayLastIndex(z -> z = WA, acc.3.1)` appears
     SIX times per step -- `LW` contains two of them and `LW` is textually
     expanded three times. (#180 §6 says "twice", which is the count inside one
     `LW`; it undercounts scan multiplicity by 3x.)
  2. the accumulator copy. `new_wl`'s three `arrayPushBack` calls on acc.3 are
     evaluated on EVERY step whether or not a store retires, because `if` does
     not short-circuit inside `arrayFold` (#183) -- an O(length) copy of
     4 + 4 + 8 = 16 bytes per element, K times per batch.

A seed sweep measures their SUM. Splitting them needs an instrument that can
vary one without the other, and -- see below -- seeding cannot be that
instrument. The split is therefore done by independent microbenchmark
(`micro.py`), not by subtraction here.

## Why lane-selective seeding does not work (measured, not reasoned)

The first design of this module had five shapes: seed the addr lane alone, the
val lane alone, the icount lane alone, or all three. The idea was that lanes
.1 and .2 are both `Array(UInt32)` and differ only in that .1 is scanned, so
`g_scan = g_V1 - g_V2` would isolate the scan.

**It is wrong, and `test_wl0_seed_is_inert_when_executed` caught it.**
`acc.3.1/.2/.3` are PARALLEL arrays: `LW` finds an index in the addr lane and
uses it to subscript the value lane --

    acc.3.2[arrayLastIndex(z -> z = WA, acc.3.1)]

-- so seeding lanes to unequal lengths desynchronises them. With the addr lane
seeded 8 deep and the val lane empty, a real store lands at addr index 9 and
val index 1; the forwarding load then reads `acc.3.2[9]` out of a one-element
array and gets the type default. Observed directly: a load that should forward
`0x1234` returned `0`.

That is a semantic change, so an unequal-lane seed is not inert and any timing
taken with one measures a *different program*. Only equal-length seeds are
valid, which is exactly why the surviving shapes are "empty" and "all lanes at
the same length".

Recorded at length because the broken version is the obvious design and the
next person will reach for it too.

## Why the seed is inert, by proof rather than by measurement

`fold._addr_and_align`'s `wa_safe` clamps the word index with
`least(..., ram_words - 1)` for EVERY address, valid or not. So a real load's
`WA` is always <= ram_words - 1, and a seeded address >= ram_words can never
satisfy `z = WA`. That is a property of the clamp, not of the address set this
particular ROM happens to touch, so it needs no empirical address analysis.

`arrayLastIndex` then returns 0 and the `acc.3.2[0]` in the guarded branch is
evaluated anyway (#183) and yields the type default without throwing. The scan
is still walked IN FULL: `arrayLastIndex` seeks the LAST match and therefore
cannot short-circuit on a miss, which is exactly the cost being measured.

The structural argument above is necessary but not sufficient -- it is what
the executed test in `executor/tests/test_fold.py` exists to check, since an
argument that is never run is indistinguishable from one that is wrong
(Non-negotiable #5). It has already earned that: see the lane-selective
section.

## Why the query text does not grow with L0

`arrayResize(empty..., L0, fill)` is a constant-size expression -- only the
decimal digits of L0 change. A literal array of L0 elements would inflate the
generated SQL (already ~59 KB, and #222 notes it sits at 60% of
`log_queries_cut_to_length`) and would make parse cost a function of L0, which
is precisely the confound the K=0 probe exists to rule out.
"""

# Any value >= config.RAM_WORDS_DEFAULT is unmatchable per the clamp argument
# above. UInt32::MAX is used because it is unmistakable in a dump: a row at
# word address 4,294,967,295 is a seed leak, never a real store.
SENTINEL_ADDR = 4_294_967_295

#: The only two semantically valid shapes -- see the module docstring on why
#: the lane-selective ones were removed rather than fixed.
SHAPES = ("V0", "VA")

SHAPE_DOC = {
    "V0": "baseline: empty write-log, byte-identical to the production seed",
    "VA": "all three lanes seeded to L0, index-aligned -- the full per-element cost",
}


def seed_sql(shape, l0):
    """The acc.3 initial value for `shape` at seed length `l0`.

    Returns SQL of constant size in `l0`. All three lanes are always seeded to
    the SAME length: they are parallel arrays indexed by a common position, so
    unequal lengths are a semantic change rather than a cost-only one.
    """
    if shape not in SHAPES:
        raise ValueError(f"unknown shape {shape!r}; expected one of {SHAPES}")
    if l0 < 0:
        raise ValueError(f"l0 must be non-negative, got {l0}")
    # V0 ignores l0 entirely -- it is the production seed by definition, and
    # silently honouring l0 here would make "V0 at L0=80000" mean something,
    # which it must not.
    n = 0 if shape == "V0" else l0
    if n == 0:
        return "tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64())"
    return ("tuple("
            f"arrayResize(emptyArrayUInt32(), {n}, toUInt32({SENTINEL_ADDR})), "
            f"arrayResize(emptyArrayUInt32(), {n}, toUInt32(0)), "
            f"arrayResize(emptyArrayUInt64(), {n}, toUInt64(0)))")


def seeded_len(shape, l0):
    """How many entries the write-log starts with -- what the HWM check sees.

    `hits_hwm` tests `length(acc.3.1) + 1 >= hwm`, so a seeded arm consumes
    high-water-mark headroom and the sweep must raise `hwm` accordingly.
    """
    return 0 if shape == "V0" else l0
