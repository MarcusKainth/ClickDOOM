# ADR-0005: The reference emulator is Rust, and the contract it shares is data

**Status:** accepted

## Context

`refemu` is the oracle. The differential runner compares its checkpoint trace
against the SQL CPU's, the milestone run compares against a trace it generated,
and the Definition of Victory is an equality between its final frame hash and
the SQL CPU's.

In Python it runs at 1.02M instructions/sec. That number sets the price of
every question the project asks it. Regenerating the `demo3` reference trace
costs 37.6 minutes, measured at 2,300,210,133 instructions in 2,253.87 seconds
(`refemu/reference_traces/demo3/demo3.9a6a47d01119.json`). A harness that
expensive needs pickle snapshots, a progress file and signal handling to survive
being interrupted. The render fixture costs 20 seconds of CI on every pull
request. Per-symbol profiling of 300 frames costs 6 minutes.

The cost lands hardest on the checks that matter most. A full-length check
nobody can afford to run is, in this repository's terms, a check that never ran.

Separately, `executor/config.py` hand-mirrors constants that `refemu` also
declares: the region bases and sizes, the MMIO offsets, the elastic-time
constant, the checkpoint intervals and the halt-reason spellings. Two
transcriptions of one contract drift, and a drift there is invisible until a
differential run disagrees for a reason that has nothing to do with either CPU.

## Decision

The reference emulator is Rust, in a Cargo workspace at the repository root.

The workspace has two members. `spec/`, the crate `clickdoom-spec`, holds what
the contract states: the memory map, the MMIO register offsets, the halt-reason
vocabulary, the checkpoint intervals and format, the three hash functions, and
the ROM manifest and its pinned hash. `refemu/` holds the machine.

**`clickdoom-spec` carries no decode and no execute logic, ever.** The oracle
means something only because it is an independent implementation of the CPU, so
a future Rust component on the SQL side links `spec/` and never `refemu/`.
Sharing the constants removes a transcription hazard. Sharing the semantics
would remove the signal.

The interpreter caches decoded instructions over the read-only text region. The
SQL CPU pre-decodes text into a table for its own reasons (ADR-0002), so the
two engines now resemble each other in the one place a shared mistake could
hide. The cache is therefore a private acceleration with a proof obligation:
cached and uncached execution agree instruction for instruction, held by a test
that runs the same program both ways and compares the full checkpoint trace,
and by a fuzz target that searches for a program where they do not.

## Consequences

The repository gains a second toolchain, a second lockfile ecosystem and a
compile step in CI. `rust-toolchain.toml` pins the version so a local `make
lint` and CI agree on which `clippy` is speaking.

`spec/` is a contract, so it joins `SPEC.md` in `.github/CODEOWNERS` and a
change to it takes the `spec` commit scope.

The sharpest cost is what the switch removes. Every artifact the port is checked
against was produced by the Python: the committed traces, the `demo3` manifest,
and the oracle a differential fuzzer compares against during the migration. A
mistake faithfully transliterated from Python passes all of them. Once the
Python is deleted, the only checks on the Rust with independent authority are
the riscv-tests fixtures, built from upstream at a pinned revision, and the SQL
CPU itself. That makes the nightly deep differential run the sole outside
opinion rather than optional confirmation.

The rollback anchor is an annotated tag, `refemu-python-final`, on the commit
before the deletion. A later disagreement between the Rust and the SQL CPU is
diagnosed three ways rather than two: check out the tag, run the Python at the
same instruction count, and read which engine moved.

Rejected alternatives. PyPy or `mypyc` keeps one language and buys five to ten
times, not two hundred, and PyPy is a second toolchain anyway. A C reference
gives up memory safety in the one component whose job is to be obviously
correct. Keeping the Python and accepting the 37-minute `demo3` run leaves the
strongest available correctness check as something a human has to decide to
start.
