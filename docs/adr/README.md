# Architecture Decision Records

Short records of decisions that should not be re-litigated from scratch. One
file per decision, named `NNNN-slug.md`, with Status, Context, Decision and
Consequences.

Write one whenever a choice crosses components, trades performance against
purity or simplicity, or reverses an earlier record.

An accepted record is immutable. Superseding one is a new record that says so,
not an edit to the old one, because the value of the set is that it shows what
was believed at the time.

    make adr-new SLUG=some-decision
    make check-adr

## The records

- [ADR-0001](0001-batch-execution-with-arrayfold.md) — Batch CPU execution via
  `arrayFold` with write-log memory. Amended by ADR-0002 and ADR-0004.
- [ADR-0002](0002-predecoded-instruction-table.md) — Pre-decoded instruction
  table, and immutable text.
- [ADR-0003](0003-batch-commit-atomicity.md) — Batch commit atomicity via a
  single atomic row plus idempotent derivation.
- [ADR-0004](0004-halt-semantics-throughput-cost.md) — Halt and bounds checking
  is a real, measured throughput cost. Amends ADR-0001's threshold.
- [ADR-0005](0005-rust-reference-emulator.md) — The reference emulator is Rust,
  and the contract it shares with the SQL side is data and never semantics.
- [ADR-0006](0006-native-mode-resident-insert-select.md) — Native mode runs as
  resident streaming `INSERT SELECT` statements, analysed once per session.
