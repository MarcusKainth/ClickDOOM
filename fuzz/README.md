# fuzz/

Coverage-guided targets for the parts of the emulator that take input it did
not write, and for the one acceleration that owes a proof.

These answer questions no oracle is needed for, which is why they are separate
from the differential against the SQL CPU. That one compares two
implementations and runs at twenty thousand cases a second. These check a
property against itself and run at a hundred thousand, which is what makes the
coverage guidance worth having.

| Target | What it checks |
|---|---|
| `predecode_equivalence` | A decoded-instruction cache changes speed and nothing else |
| `step_invariants` | The machine holds its own invariants whatever it is fed |
| `elf_loader` | A malformed ELF is an error, never a panic or an unbounded read |
| `snapshot_reader` | A malformed capture is an error, and a good one round-trips |

`predecode_equivalence` is the one that pays for the rest. The cache covers the
read-only region, and the interesting cases are a jump landing inside it and
execution straddling its end. Structured random generation mostly does not
produce those; coverage guidance does.

    make fuzz                  # every target, a minute each
    make fuzz TARGET=elf_loader FUZZ_SECONDS=300

This crate is detached from the root workspace and pins its own nightly,
because the sanitizer flags libFuzzer needs are nightly-only.

A case that fails is written to `fuzz/artifacts/`. Add it to the target's
`corpus/` directory once it is understood, so the finding runs from then on
whether or not anyone fuzzes again.
