# ADR-0006: Native mode runs as resident streaming INSERT SELECT statements

**Status:** accepted

## Context

Native mode reimplements DOOM's tic simulation and renderer as ClickHouse SQL
and has to deliver a frame every 28.6 ms (35 Hz). On the pinned ClickHouse
(26.7.5.10) every query pays parse, analysis and planning at about 25 µs per
AST node: a 1,000-node statement costs 24 ms before it executes, and the CPU
fold's 90,000-node statement costs 1,650 ms (`docs/experiments/batch-attribution.md`).
There is no plan cache and no prepared statement; SQL user-defined functions
and parameterized views are inlined and re-analysed on every query. A
simulation or renderer statement is tens of thousands of nodes, so issuing
one per tic cannot reach 35 Hz on any hardware.

A long-lived `INSERT INTO <table> SELECT <transform> FROM input(...)` over one
chunked HTTP body behaves differently. It is analysed once. With
`max_insert_block_size = 1` and its sibling settings, every streamed row is
its own block and is processed as it arrives: send-to-visible latency
measured at 1.0 ms median over 200 rows at 35 Hz. When the destination is a
`Join`-engine table, a row written by block t is readable through `joinGet`
by block t+1 of the same running statement, which is how state carries from
tic to tic. `Memory`-engine destinations commit only when the statement ends
and are unusable for this. Two such statements chained through a `Join` table
(simulation, then renderer) completed a tic in 2.4 ms median with trivial
transforms.

## Decision

Each native-mode component is one resident statement per session: the
simulation writes `native_state`, the renderer writes `native_frames`, both
`Join`-engine tables keyed by tic or frame. The driver streams one small row
per tic into each body and reads results back with `joinGet`. Static level
data enters a statement as scalar subquery constants, which ClickHouse
evaluates once per statement. The statement text leads the request body,
because a URL parameter is limited to about 64 KB; settings that must be known
before parsing (`max_query_size`, the block-size settings) travel as URL
parameters rather than a `SETTINGS` clause. The server's `http_receive_timeout` is raised by a
mounted configuration file so an idle session survives.

## Consequences

Per-tic cost is execution only, so the budget is spent on the transform and
not on the analyzer. A streamed statement cannot use `GROUP BY`, `ORDER BY`,
window functions or a recursive CTE, because those wait for end of input;
every stage is an array expression inside the single input row. The driver
must send row t+1 only after row t is visible, or `joinGet` of the previous
tic reads nothing. A statement that dies is reopened and resumes from the
last committed tic; `Join` tables persist to disk. The alternative, one query
per tic, was rejected on the measurement above; the native TCP protocol was
not needed once HTTP streaming was shown to work.
