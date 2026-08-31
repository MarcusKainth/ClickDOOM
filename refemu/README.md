# refemu/

The reference RV32IM interpreter, in Rust. It is the oracle: the known-good
run that the SQL implementation is checked against, instruction by instruction.

It must stay an independent implementation. It shares no code with `sqlcpu/`,
which is the only reason a disagreement between the two means anything.

`SPEC.md` defines the CPU, the memory map, the MMIO surface and the trace
format. This directory implements them, and emits the checkpoint traces the
differential runner compares.

## Tests

    cargo test --workspace

No ClickHouse and no ROM. The suite covers the committed riscv-tests fixtures,
the device model, halt semantics, the decode cache's equivalence with decoding
on every fetch, and the formats.

    make test

Also regenerates the committed traces from the pinned ROM and compares them, and
runs the whole `demo3` demo against the values its manifest records. Needs
`make build-rom` first.

    make fuzz

Coverage-guided fuzzing. See `../fuzz/README.md`.

## Reference traces

`reference_traces/` holds committed traces, each named after the ROM it came
from so a re-pinned ROM cannot silently reuse the previous one's.
[Its README](reference_traces/README.md) records what each one is.

Regenerating one is `make gen-reference-trace`, which refuses to run against an
unpinned ROM. The interpreter runs at about 170 million instructions per second
on a quiet machine, so the whole `demo3` run regenerates in under twenty
seconds.
