# B1: native ClickHouse vs Docker Desktop

Runs `rom/bench/canonical_throughput/run.sh` unchanged against three fresh servers.

| arm | version | platform |
|---|---|---|
| A | 26.3.17.4 | Docker |
| B | 26.3.25.2 | Docker |
| C | 26.3.25.2 | native macOS `clickhouse server` |

B minus A is the patch-version effect. C minus B is the Docker VM effect.

## Native binary

GitHub release `v26.3.25.2-lts`, asset `clickhouse-macos-aarch64`.
Downloaded: 161,185,045 bytes, sha256 `77a22fe681807ca3e8614a0eea13b26a57d7b5739f3a12be03376d7ced104533`.
The binary self-extracts on first run. After that it is 855,405,159 bytes, sha256 `0133ca5cfd5dfcff0d8c65003955323e05d0cb7ee708abaf752a1d028aa44610`.

Install it at `~/.clickhouse/26.3.25.2/clickhouse`, or set `CLICKDOOM_NATIVE_CLICKHOUSE`.
The scripts never use `clickhouse` from `PATH`.
The same binary is the client for all three arms.

## Run

    # take the machine lock (kind: timing) first
    make bench-native            # 3 repeats, 3 arms, 3 batches per mode and window
    make bench-native REPEATS=1 BATCHES=1 ARMS=C      # smoke test: one batch, native only

Arm order rotates per repeat (ABC, BCA, CAB).
Output goes to `$TMPDIR/clickdoom-b1-native/<stamp>/`. `results.tsv` has one row per repeat, arm, window and mode.

The native server uses ports 9100 and 8223 and state under `$TMPDIR/clickdoom-native-ch`.
`native_server.sh fresh` wipes that state so the compiled-expression cache starts cold.
