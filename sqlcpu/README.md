# sqlcpu/

Instruction decode and execute, as ClickHouse SQL. This is the CPU.

Everything here runs inside the database: the register file, RAM and every
load and store, the decoded-instruction table, MMIO state, and the checkpoint
hashes. `PURITY.md` states what that means and what may not leave it.

Decoding the ROM inside ClickHouse is allowed; doing it in Python and inserting
the result is not, which is PUR-11. That is why the decode step is SQL rather
than a script. The Python here generates SQL text and executes nothing itself.

## Tests

    make test-sqlcpu

Needs a live ClickHouse, which the target starts. It runs, in sequence, the
riscv-tests inside the database, the committed decode vectors, the execute
checks, and the checkpoint checks against the reference interpreter's worked
examples.
