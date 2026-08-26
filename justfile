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

# riscv-tests INSIDE ClickHouse — sqlcpu workstream
test-sqlcpu: up
    @test -f sqlcpu/schema.sql || { echo "sqlcpu/ not landed yet"; exit 1; }
    ./sqlcpu/run_tests.sh --host localhost --port 9000

# Fold unit tests against the SPEC §5-shaped fixture schema — executor workstream
test-executor: up
    cd executor && uv run pytest tests/ -v

# Differential run of N instructions; reports first divergence (SPEC §7)
diff N: up
    ./scripts/diff_run.sh {{N}}

# CI-sized differential smoke
smoke: (diff "1000000")

# Phase 0 arrayFold characterisation benchmark (ADR-0001/0002 evidence).
# One-off harness, not the nightly regression gate — that is `just bench`.
bench-phase0: up
    ./executor/bench/phase0/run.sh

# Executor throughput benchmark (instructions/sec)
bench: up
    @test -f executor/bench.sh || { echo "executor/ not landed yet"; exit 1; }
    ./executor/bench.sh --host localhost --port 9000

# Play: the driver loop (Phase 2+)
run: up
    @test -f driver/main.py || { echo "driver/ not landed yet"; exit 1; }
    cd driver && uv run python main.py

# All linters + purity check (what CI runs)
lint:
    ./scripts/check_purity.sh
    find scripts driver -name '*.sh' -print0 2>/dev/null | xargs -0 -r shellcheck
    @if [ -f refemu/pyproject.toml ] || [ -f driver/pyproject.toml ]; then ruff check refemu driver; fi
