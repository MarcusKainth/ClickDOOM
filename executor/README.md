# executor/

The batch loop. It executes many instructions per statement rather than one
round trip each, which is what makes a full run finish in a useful amount of
time.

It generates the `arrayFold` batch statement, with the step expression, the halt
checks and the write-log; and the flushes that merge the write-log into `ram`
and derive the `cpu_state` row. `SPEC.md` defines the batch execution contract
they implement.

Generating SQL text is not computation. What the SQL then does is, and all of it
stays in the database.

## Tests

    make test-executor

Needs a live ClickHouse, which the target starts. Covers the fold, the commit
path and MMIO against a fixture schema, so the tests need no ROM run.

## Benchmarks

[docs/benchmarks.md](../docs/benchmarks.md) says
what each one settles and where its findings are.
