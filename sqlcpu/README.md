# sqlcpu/

Instruction decode and execute, as ClickHouse SQL. This is the CPU.

Everything here runs inside the database: the register file, RAM and every
load and store, the decoded-instruction table, MMIO state, and the checkpoint
hashes. `PURITY.md` states what that means and what may not leave it.

Decoding the ROM inside ClickHouse is allowed; doing it outside the database
and inserting the result is not, which is PUR-11. That is why the decode step
is SQL rather than a script. `schema.sql` and `decode.sql` are the CPU, and
`clickdoom emulation decode` only runs them.

## Tests

    make test

Needs a live ClickHouse, which the target starts. `driver/tests/sqlcpu_live.rs`
is the suite: the committed decode vectors against `decode.sql`, one
instruction at a time against an independent RV32I reference, the riscv-tests
corpus run to completion inside the database, and the checkpoint expressions
against `clickdoom-spec`'s own hashes. Every instruction executes through the
fold `executor/src/fold.rs` builds, which is the fold the DOOM run itself
folds.
