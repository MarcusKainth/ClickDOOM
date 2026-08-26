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

# All linters + purity check (what CI runs)
#
# NOTE: CI's lint job (.github/workflows/ci.yml) only shellchecks scripts/ and
# driver/ — it predates sqlcpu/executor landing shell scripts and is a
# workflow file (human gate), so this recipe covers the gap locally rather
# than editing it here. Flagged to the team lead as a ci: follow-up.
lint:
    ./scripts/check_purity.sh
    find scripts driver sqlcpu executor rom -name '*.sh' -print0 2>/dev/null | xargs -0 -r shellcheck
    @if [ -f refemu/pyproject.toml ] || [ -f driver/pyproject.toml ]; then ruff check refemu driver; fi
