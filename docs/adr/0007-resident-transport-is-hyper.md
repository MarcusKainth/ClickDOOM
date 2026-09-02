# ADR-0007: The resident statement's transport is hyper, not the clickhouse crate

**Status:** accepted

## Context

ADR-0006 makes each native-mode component one `INSERT INTO ... SELECT ... FROM
input(...)` statement that stays open for a whole session, analysed once, with
one row streamed into its body per tic. That statement is a single HTTP
request whose body has to be written one chunk per row over minutes, with a
flush after every row so the server processes it now, and whose response has
to be read while the body is still open, because a statement that has failed
answers only when the body closes.

The driver already speaks to ClickHouse through the `clickhouse` crate, and
emulation mode uses nothing else. Its insert API buffers rows and closes the
request when the insert ends; it has no per-row flush and no way to keep a
request open between rows. Its periodic inserter ends one request and opens
the next, which would re-analyse the statement each time, the cost ADR-0006
exists to avoid. Neither shape can carry a resident statement.

## Decision

The resident statement is carried by `hyper` directly: one `http1` connection
per statement, a request body implemented as a `hyper::body::Body` that yields
one chunk per row from a channel, and a task that reads the response from the
moment the request is sent. It lives in `driver/src/native/stream.rs` and
nowhere else. Every other statement native mode issues, including the loads,
the polls and the parity queries, goes through the `clickhouse` crate as
emulation mode's do.

## Consequences

`hyper` is the transport the `clickhouse` crate already uses, so the driver
gains a direct dependency on crates it already compiled and no new tree.
Authentication and the settings that must be known before parsing are set on
the request by `stream.rs` itself, so that module carries the protocol
`NATIVE.md` states: statement text first, then a padding row, then rows. If
the `clickhouse` crate gains a streaming insert with per-row flush and an
open-ended body, `stream.rs` is the one module to replace. The alternative of
the native TCP protocol was not needed once HTTP streaming was shown to work,
and would have added a client crate the tree does not have.
