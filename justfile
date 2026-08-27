# ClickDOOM canonical commands. Agents: use these; never improvise shell
# incantations. Missing a recipe? Add it in the same PR that needs it.

set shell := ["bash", "-uc"]

default:
    @just --list

# Pinned ClickHouse up/down for local dev
up:
    docker compose up -d --wait

down:
    docker compose down

# Reproducible ROM build (dockerized rv32 toolchain) — rom workstream
build-rom:
    @test -f rom/Makefile || { echo "rom/ not landed yet (see task backlog)"; exit 1; }
    make -C rom

# riscv-tests against the Python reference emulator — refemu workstream
test-refemu:
    @test -f refemu/pyproject.toml || { echo "refemu/ not landed yet"; exit 1; }
    cd refemu && uv run pytest -q

# Regenerate refemu's committed riscv-tests fixtures (maintenance only, not part of CI)
build-riscv-tests-fixtures:
    ./refemu/scripts/build_riscv_tests.sh

# Regenerate refemu's committed SPEC §7 reference trace (maintenance only,
# not part of CI). Needs rom/build/doom-rv32im.bin + manifest.json (`just
# build-rom` first) matching rom/PINNED_HASH — the script refuses to run
# against an unpinned ROM. Cross-checks its own output against issue #29's
# independently reproduced milestones by default; a mismatch is either a
# real ROM change (update the script's --expect-* defaults) or a genuine
# refemu regression (investigate, don't suppress).
gen-reference-trace:
    cd refemu && uv run python scripts/gen_reference_trace.py

# riscv-tests INSIDE ClickHouse — sqlcpu workstream
test-sqlcpu: up
    @test -f sqlcpu/schema.sql || { echo "sqlcpu/ not landed yet"; exit 1; }
    ./sqlcpu/run_tests.sh --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# Fold unit tests against the SPEC §5-shaped fixture schema — executor workstream
test-executor: up
    cd executor && uv run pytest tests/ -v

# Frame readout (issue #29) against a fixture matching sqlcpu's proposed
# framebuffer/palette persistence shape (#160, not landed yet) -- proves
# frame_readout_sql() reproduces the real fb_hash oracle from real refemu
# data, and ansi_render_sql() byte-matches a hand-computed synthetic case.
test-render: up
    ./driver/test_render.sh --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# Differential run of N instructions; reports first divergence (SPEC §7)
diff N: up
    @test -f scripts/diff_run.sh || { echo "scripts/diff_run.sh not landed yet (executor workstream, issue #27)"; exit 1; }
    ./scripts/diff_run.sh {{N}}

# CI-sized differential smoke
smoke: (diff "1000000")

# Phase 0 arrayFold characterisation benchmark (ADR-0001/0002 evidence).
# One-off harness, not the nightly regression gate — that is `just bench`.
bench-phase0: up
    ./executor/bench/phase0/run.sh

# E7: exact per-symbol instruction attribution for the real ROM (issue #126)
# -- also a general "where do the instructions go" tool, not single-use.
# No `up` dependency: refemu-only, no ClickHouse involved.
bench-e7-memfns:
    @test -f rom/build/doom-rv32im.bin || { echo "rom/ not built yet -- run just build-rom first"; exit 1; }
    cd refemu && uv run python ../rom/bench/e7_memfns/profile_memfns.py --frames 40

# E1: does ClickHouse's arrayFold dedup repeated subexpressions, and at what
# node-count cost (issue #126) -- needs the shared clickdoom-ch container.
bench-e1-cse: up
    ./executor/bench/e1_cse/run.sh

# The canonical real-ROM throughput benchmark for the optimisation sprint
# (#147): boot-phase + store-heavy gameplay windows, fold-alone + e2e,
# K at #80's optimum, contention-checked, full provenance. Coordinate with
# whoever else might be using the shared container first -- see
# rom/bench/canonical_throughput/README.md.
bench-canonical-throughput: up
    @test -f rom/build/doom-rv32im.bin || { echo "rom/ not built yet -- run just build-rom first"; exit 1; }
    ./rom/bench/canonical_throughput/run.sh \
        --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# #180/#182: per-statement attribution of the e2e batch. Breaks the batch
# apart -- own query_id per statement, standalone RAMT/DEC/KEYQ timings, a
# direct select_only(K=0) reading of the fixed per-batch setup cost, and a
# part_log/mutations dump. Creates and destroys its OWN container per arm
# (the compiled-expression cache is server-global, #166), so it does NOT
# depend on `up` -- but nothing else should be running on the box.
bench-commit-attribution LABEL="baseline" K="60000" BATCHES="4":
    @test -f rom/build/doom-rv32im.bin || { echo "rom/ not built yet -- run just build-rom first"; exit 1; }
    ./executor/bench/commit_mutation/arm.sh --label {{LABEL}} -- --k {{K}} --batches {{BATCHES}}

# #180's fixed-instruction-window K-sweep: every arm executes the same
# instruction window, cut into a different number of batches, so the arms
# differ only by how many per-batch setups they pay. Same container-per-arm
# rule as above.
bench-ksweep WINDOW="120000":
    @test -f rom/build/doom-rv32im.bin || { echo "rom/ not built yet -- run just build-rom first"; exit 1; }
    ./executor/bench/commit_mutation/ksweep.sh --window {{WINDOW}}
    python3 executor/bench/commit_mutation/fit.py /tmp/sq2-bench/K_sweep_*.json

# Executor throughput benchmark (instructions/sec)
bench: up
    @test -f executor/bench.sh || { echo "executor/ not landed yet"; exit 1; }
    ./executor/bench.sh --host localhost --port 9000

# Play: the driver loop (Phase 2+)
run: up
    @test -f driver/main.py || { echo "driver/ not landed yet"; exit 1; }
    cd driver && uv run python main.py

# Pre-flight gate for a multi-hour run (#110's Phase 2 milestone first,
# demo3 later) — refuses to start rather than advising. K/HWM/database
# match #110's stated run parameters; override via CLICKDOOM_* env vars the
# same way executor's bench scripts do. Reference trace path is computed
# the same way refemu/scripts/gen_reference_trace.py names its own output
# (ROM-sha-prefixed, since a stale hardcoded path would point at whatever
# ROM happened to be pinned when this recipe was written, not the current
# one) — override CLICKDOOM_REFERENCE_TRACE directly if the file doesn't
# exist yet under that name.
preflight-milestone: up
    ./scripts/preflight_milestone.sh \
        --bin "${CLICKDOOM_ROM_BIN:-rom/build/doom-rv32im.bin}" \
        --manifest "${CLICKDOOM_ROM_MANIFEST:-rom/build/manifest.json}" \
        --k "${CLICKDOOM_RUN_K:-60000}" \
        --hwm "${CLICKDOOM_RUN_HWM:-20000}" \
        --database "${CLICKDOOM_DATABASE:-clickdoom}" \
        --trace "${CLICKDOOM_REFERENCE_TRACE:-refemu/reference_traces/demo-boot-to-first-frame.$(cut -c1-12 rom/PINNED_HASH).tsv}" \
        --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# The resumable batch-loop runner itself (#110's Phase 2 milestone: first
# FRAME_COMMIT). Same CLICKDOOM_* overrides as preflight-milestone above,
# plus CLICKDOOM_TARGET_ICOUNT (default: #110's ratified FRAME_COMMIT
# target -- 15,393,136 as of #175's R_DrawColumn/R_DrawSpan unroll,
# PINNED_HASH 9a6a47d0...; the fb_hash at that icount, fe5d82c0f42d45f1,
# is unchanged by #175 -- re-derived directly from a live refemu run
# against the current PINNED_HASH, not carried over by arithmetic on the
# pre-unroll number). Calls preflight_milestone.sh internally and refuses
# to start if it fails -- do not run preflight-milestone separately first,
# it is redundant. **Do not run this against the shared `clickdoom`
# database for real without team-lead sign-off** -- #25/#29 gate the
# actual milestone run (#110); this recipe exists so the instrument itself
# is one command, not so it is safe to fire at any time.
run-milestone: up
    ./scripts/run_milestone.sh \
        --bin "${CLICKDOOM_ROM_BIN:-rom/build/doom-rv32im.bin}" \
        --manifest "${CLICKDOOM_ROM_MANIFEST:-rom/build/manifest.json}" \
        --k "${CLICKDOOM_RUN_K:-60000}" \
        --hwm "${CLICKDOOM_RUN_HWM:-20000}" \
        --database "${CLICKDOOM_DATABASE:-clickdoom}" \
        --trace "${CLICKDOOM_REFERENCE_TRACE:-refemu/reference_traces/demo-boot-to-first-frame.$(cut -c1-12 rom/PINNED_HASH).tsv}" \
        --target-icount "${CLICKDOOM_TARGET_ICOUNT:-15393136}" \
        --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# Same as run-milestone, but for a target icount past the first
# FRAME_COMMIT. run_milestone.sh stops at every FRAME_COMMIT, not just at
# --target-icount (#210) -- reaching a later frame means re-invoking it
# once per intervening frame. This recipe does that via
# scripts/run_milestone_through_frames.sh, a pure driver loop with no
# logic beyond "re-run the same command, read the icount it already
# printed" -- see that script's header for the full story. Same
# CLICKDOOM_* overrides as run-milestone above; same sign-off requirement.
run-milestone-through-frames: up
    ./scripts/run_milestone_through_frames.sh \
        --bin "${CLICKDOOM_ROM_BIN:-rom/build/doom-rv32im.bin}" \
        --manifest "${CLICKDOOM_ROM_MANIFEST:-rom/build/manifest.json}" \
        --k "${CLICKDOOM_RUN_K:-60000}" \
        --hwm "${CLICKDOOM_RUN_HWM:-20000}" \
        --database "${CLICKDOOM_DATABASE:-clickdoom}" \
        --trace "${CLICKDOOM_REFERENCE_TRACE:-refemu/reference_traces/demo-boot-to-first-frame.$(cut -c1-12 rom/PINNED_HASH).tsv}" \
        --target-icount "${CLICKDOOM_TARGET_ICOUNT:-15393136}" \
        --host localhost --port 9000 --password "${CLICKHOUSE_PASSWORD:-clickdoom}"

# All linters + purity check (what CI runs)
lint:
    ./scripts/check_purity.sh
    find scripts driver sqlcpu executor rom -name '*.sh' -print0 2>/dev/null | xargs -0 -r shellcheck
    @if [ -f refemu/pyproject.toml ] || [ -f driver/pyproject.toml ]; then ruff check refemu driver; fi
