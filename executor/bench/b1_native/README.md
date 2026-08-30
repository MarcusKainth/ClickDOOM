# B1: native ClickHouse vs Docker Desktop

A closed investigation into native macOS ClickHouse against Docker Desktop,
across three server/version combinations:

| arm | version | platform |
|---|---|---|
| A | 26.3.17.4 | Docker |
| B | 26.3.25.2 | Docker |
| C | 26.3.25.2 | native macOS `clickhouse server` |

B minus A is the patch-version effect. C minus B is the Docker VM effect.

[`RESULTS.md`](RESULTS.md) carries the full record and its conclusion: the
run box stays on Docker, native macOS ClickHouse is rejected for this
workload.
