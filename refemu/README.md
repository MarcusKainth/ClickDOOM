# refemu/

The reference RV32IM interpreter, in Python. It is the oracle: the known-good
run that the SQL implementation is checked against, instruction by instruction.

It must stay an independent implementation. It shares no code with `sqlcpu/`,
which is the only reason a disagreement between the two means anything.

`SPEC.md` defines the CPU, the memory map, the MMIO surface and the trace
format. This directory implements them, and emits the checkpoint traces the
differential runner compares.

## Tests

    make test-refemu

No ClickHouse needed. The suite covers the committed riscv-tests fixtures, the
MMIO devices, halt semantics, and the trace emitters.

## Reference traces

`reference_traces/` holds committed traces, each named after the ROM it came
from so a re-pinned ROM cannot silently reuse the previous one's.
[Its README](reference_traces/README.md) records what each one is.

Regenerating one is `make gen-reference-trace`, which refuses to run against an
unpinned ROM. The interpreter runs at roughly a million instructions per second,
which sets the cost.
