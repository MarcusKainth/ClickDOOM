# ClickDOOM task runner. `make help` lists every target.
#
# Editing rules:
#
#   No `@` prefix on a target that checks something, so the log shows the real
#   invocation. `help` is the exception.
#
#   No pipes. A pipeline reports only the last command's exit status, so a
#   failure upstream of the last stage passes silently. `.SHELLFLAGS` sets
#   pipefail for the pipes inside the scripts these targets call.
#
#   One `.PHONY` list, below. Nothing here builds a file of its own name.
#
# Not parallel-safe. Most targets share one ClickHouse container, and the
# compiled-expression cache is server-global, so two timing runs at once
# measure each other. Do not pass -j.

.DEFAULT_GOAL := help
SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c

# Connection. Environment values win, which is how CI passes its own.
CLICKHOUSE_PASSWORD ?= clickdoom
CH_HOST ?= localhost
CH_PORT ?= 9000

# The command that speaks the native protocol. Empty means each script picks
# its own default. Set it when the host has no `clickhouse-client` on PATH:
#
#   make test-render CH_CLIENT="docker exec -i clickdoom-ch clickhouse-client"
CH_CLIENT ?=
client_flag = $(if $(CH_CLIENT),--client "$(CH_CLIENT)",)

conn = --host $(CH_HOST) --port $(CH_PORT) --password "$(CLICKHOUSE_PASSWORD)"

# clickhouse-client blocks forever on an INSERT when stdin is an open pipe
# rather than at EOF. Every target here is non-interactive, so stdin is closed
# for all of them. Without it `make diff` inside a pipeline, an editor task
# runner or a CI step hangs with no output and no query running server-side.
no_stdin = < /dev/null

# Where the reference trace lands. Named after the ROM it was generated from,
# the same way the generator names its own output, so a re-pinned ROM does not
# silently reuse the previous one's trace.
ROM_BIN ?= rom/build/doom-rv32im.bin
ROM_MANIFEST ?= rom/build/manifest.json
CLICKDOOM_DATABASE ?= clickdoom
CLICKDOOM_RUN_K ?= 60000
CLICKDOOM_RUN_HWM ?= 20000
CLICKDOOM_TARGET_ICOUNT ?= 15393136

# The reference emulator, and the numbers a regenerated trace is checked
# against. These move when rom/PINNED_HASH does, and they live here so the
# ROM's hash and the milestones it implies sit together.
REFEMU ?= ./target/release/refemu
REFERENCE_TRACE_MAX ?= 15728640
EXPECT_INIT_GRAPHICS ?= 11014966
EXPECT_FIRST_FRAME_FBHASH ?= fe5d82c0f42d45f1
DEMO3_MAX ?= 4000000000
reference_trace = refemu/reference_traces/demo-boot-to-first-frame.$$(cut -c1-12 rom/PINNED_HASH).tsv

.PHONY: help up down build-rom \
        test test-refemu test-refemu-rust test-refemu-python \
        test-sqlcpu test-executor test-render smoke diff \
        bench-phase0 bench-e1-cse bench-e7-memfns bench-canonical-throughput \
        bench-native bench-commit-attribution bench-ksweep bench-wl-seed \
        bench-batch-overhead bench-halt-overhead bench-hwm bench-a1-jit \
        bench-b2-block-dispatch bench-b3-dict-lookup \
        preflight-milestone run-milestone \
        build-refemu build-riscv-tests-fixtures gen-reference-trace gen-demo3-trace \
        fuzz fuzz-refemu-vs-python fuzz-refemu-selftest \
        lint check-purity shellcheck ruff cargo-fmt cargo-clippy \
        clang-format typos actionlint zizmor \
        adr-new check-adr require-rom

help: ## List every target
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage: make <target>\n"} \
		/^[a-zA-Z0-9_-]+:.*?##/ { printf "  \033[36m%-28s\033[0m %s\n", $$1, $$2 } \
		/^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) }' $(MAKEFILE_LIST)
	@echo

##@ Environment

up: ## Start the pinned ClickHouse container
	docker compose up -d --wait

down: ## Stop it
	docker compose down

##@ Build

build-rom: ## Build the DOOM ROM reproducibly, in the pinned toolchain image
	make -C rom

build-refemu: ## Build the reference emulator
	cargo build --locked --release -p refemu

require-rom:
	test -f $(ROM_BIN) || { echo "$(ROM_BIN) missing. Run: make build-rom" >&2; exit 1; }

##@ Test

test: test-refemu test-sqlcpu test-executor test-render ## Every suite that has one

test-refemu: test-refemu-rust test-refemu-python ## Every reference-emulator suite. No ClickHouse, no ROM

test-refemu-rust: ## The Rust suites: units, the riscv-tests fixtures, the formats
	cargo test --locked --workspace

test-refemu-python: ## The Python interpreter's own suite, until it is removed
	cd refemu && uv run pytest -q

test-sqlcpu: up ## riscv-tests inside ClickHouse
	./sqlcpu/run_tests.sh $(conn) $(no_stdin)

test-executor: up ## Fold, commit and MMIO unit tests
	cd executor && uv run pytest tests/ -v $(no_stdin)

test-render: up ## Frame readout and the ANSI/PPM render queries
	./driver/test_render.sh $(conn) $(client_flag) $(no_stdin)

N ?= 100000
diff: up ## Differential run of N instructions, reporting the first divergence
	./scripts/diff_run.sh $(N) $(conn) $(client_flag) $(no_stdin)

smoke: ## The differential run CI uses, at 100,000 instructions
	$(MAKE) diff N=100000

##@ Bench
#
# Timings need a quiet machine. Several of these create and destroy their own
# container per arm, because the compiled-expression cache is server-global and
# would otherwise carry state between arms.

bench-phase0: up ## arrayFold characterisation, the evidence behind ADR-0001 and ADR-0002
	./executor/bench/phase0/run.sh $(no_stdin)

bench-e1-cse: up ## Does arrayFold deduplicate repeated subexpressions, and at what node cost
	./executor/bench/e1_cse/run.sh $(no_stdin)

bench-e7-memfns: require-rom ## Per-symbol instruction attribution for the real ROM. No ClickHouse
	cd refemu && uv run python ../rom/bench/e7_memfns/profile_memfns.py --frames 40

bench-canonical-throughput: up require-rom ## Real-ROM throughput: boot and gameplay windows, fold-alone and end to end
	./rom/bench/canonical_throughput/run.sh \
		--bin $(ROM_BIN) --manifest $(ROM_MANIFEST) $(conn) $(no_stdin)

REPEATS ?= 3
BATCHES ?= 3
ARMS ?= ABC
bench-native: require-rom ## Native ClickHouse against Docker. Starts its own servers on 9010, 9020, 9100
	./executor/bench/b1_native/run.sh --repeats $(REPEATS) --batches $(BATCHES) --arms $(ARMS) $(no_stdin)

LABEL ?= baseline
K ?= 60000
bench-commit-attribution: require-rom ## Per-statement attribution of one end-to-end batch. Own container per arm
	./executor/bench/commit_mutation/arm.sh --label $(LABEL) -- --k $(K) --batches $(BATCHES) $(no_stdin)

WINDOW ?= 120000
bench-ksweep: require-rom ## Fixed instruction window cut into different batch counts, so arms differ only by setup count
	./executor/bench/commit_mutation/ksweep.sh --window $(WINDOW) $(no_stdin)
	python3 executor/bench/commit_mutation/fit.py /tmp/sq2-bench/K_sweep_*.json $(no_stdin)

DB ?= wl257_boot
REPS ?= 5
bench-wl-seed: require-rom ## Does per-instruction cost grow within a batch? Seeds the write-log and reads the slope directly
	./executor/bench/commit_mutation/setup_db.sh --container clickdoom-ch --db $(DB) --window boot $(no_stdin)
	python3 executor/bench/wl_seed/micro.py --out /tmp/wl257-micro.json $(no_stdin)
	python3 executor/bench/wl_seed/bench_l0.py --db $(DB) --k 60000 --reps $(REPS) --out /tmp/wl257-k60000.json $(no_stdin)
	python3 executor/bench/wl_seed/bench_l0.py --db $(DB) --k 30000 --reps $(REPS) --out /tmp/wl257-k30000.json $(no_stdin)
	python3 executor/bench/wl_seed/bench_l0.py --db $(DB) --k 15000 --reps $(REPS) --out /tmp/wl257-k15000.json $(no_stdin)
	python3 executor/bench/wl_seed/fit_l0.py /tmp/wl257-k*.json --micro /tmp/wl257-micro.json $(no_stdin)

bench-batch-overhead: up ## Fixed per-batch cost, with its own query_log window guard
	./executor/bench/batch_overhead/run.sh $(no_stdin)

bench-halt-overhead: up ## What the halt check costs per batch, swept over K
	./executor/bench/halt_overhead/run.sh $(no_stdin)

bench-hwm: up ## Write-log high-water mark, swept over K
	./executor/bench/hwm/run.sh $(no_stdin)

bench-a1-jit: up ## Which parts of the fold expression ClickHouse's JIT compiles, and what that buys
	cd executor/bench/a1_jit && ./run.sh $(no_stdin)

bench-b2-block-dispatch: ## What an unselected branch costs inside arrayFold
	./executor/bench/b2_block_dispatch/run.sh $(no_stdin)

bench-b3-dict-lookup: ## dictGet against arrayElement for RAM reads in the fold
	./executor/bench/b3_dict_lookup/run.sh $(no_stdin)

##@ Milestone

preflight-milestone: up ## Fail-closed gates before a multi-hour run. Refuses to start rather than advising
	./scripts/preflight_milestone.sh \
		--bin "$(ROM_BIN)" --manifest "$(ROM_MANIFEST)" \
		--k "$(CLICKDOOM_RUN_K)" --hwm "$(CLICKDOOM_RUN_HWM)" \
		--database "$(CLICKDOOM_DATABASE)" \
		--trace "$(reference_trace)" \
		$(conn) $(no_stdin)

run-milestone: up ## The resumable batch loop. Runs its own preflight and refuses to start if it fails
	./scripts/run_milestone.sh \
		--bin "$(ROM_BIN)" --manifest "$(ROM_MANIFEST)" \
		--k "$(CLICKDOOM_RUN_K)" --hwm "$(CLICKDOOM_RUN_HWM)" \
		--database "$(CLICKDOOM_DATABASE)" \
		--trace "$(reference_trace)" \
		--target-icount "$(CLICKDOOM_TARGET_ICOUNT)" \
		--stop-at-frame 0 \
		$(conn) $(no_stdin)

##@ Maintenance

FUZZ_CASES ?= 200000
FUZZ_SEED ?= 1
fuzz-refemu-vs-python: ## Compare the Rust interpreter against the Python one over generated cases
	FUZZ_CASES=$(FUZZ_CASES) FUZZ_SEED=$(FUZZ_SEED) \
	    cargo test --locked --release --features refemu/py-oracle \
	    --test py_differential -- --nocapture --test-threads=1

fuzz-refemu-selftest: ## Show the differential failing, against a deliberately broken build
	cargo test --locked --release --features refemu/py-oracle,refemu/fuzz-selftest \
	    --test py_differential the_differential_catches -- --nocapture

FUZZ_SECONDS ?= 60
FUZZ_TARGETS ?= predecode_equivalence step_invariants elf_loader snapshot_reader
fuzz: ## Coverage-guided fuzzing. Needs the nightly fuzz/ pins and cargo-fuzz
	for target in $(FUZZ_TARGETS); do \
	    echo "== $$target =="; \
	    (cd fuzz && cargo +nightly fuzz run "$$target" -- \
	        -max_total_time=$(FUZZ_SECONDS) -print_final_stats=1); \
	done

build-riscv-tests-fixtures: ## Regenerate refemu's committed riscv-tests fixtures
	./refemu/scripts/build_riscv_tests.sh

gen-reference-trace: require-rom build-refemu ## Regenerate the committed reference trace. Refuses to run against an unpinned ROM
	$(REFEMU) run $(ROM_BIN) --manifest $(ROM_MANIFEST) \
	    --pinned-hash rom/PINNED_HASH \
	    --stop-at frame:0 -n $(REFERENCE_TRACE_MAX) \
	    --expect-icount $(CLICKDOOM_TARGET_ICOUNT) \
	    --expect-fbhash $(EXPECT_FIRST_FRAME_FBHASH)
	$(REFEMU) trace $(ROM_BIN) --manifest $(ROM_MANIFEST) \
	    --pinned-hash rom/PINNED_HASH -n $(REFERENCE_TRACE_MAX) \
	    --console-milestone 'I_InitGraphics: framebuffer=init_graphics' \
	    --expect-milestone init_graphics=$(EXPECT_INIT_GRAPHICS) \
	    --out-dir refemu/reference_traces --name demo-boot-to-first-frame

gen-demo3-trace: require-rom build-refemu ## Run demo3 to completion and write its manifest. The .tsv is not committed
	$(REFEMU) trace $(ROM_BIN) --manifest $(ROM_MANIFEST) \
	    --pinned-hash rom/PINNED_HASH --stop-at halt -n $(DEMO3_MAX) \
	    --out-dir refemu/reference_traces/demo3 --name demo3

##@ Docs

SLUG ?=
adr-new: ## Scaffold an ADR: make adr-new SLUG=some-decision
	./scripts/adr.sh --new "$(SLUG)"

check-adr: ## The ADR set is numbered contiguously and fully indexed
	./scripts/adr.sh --check

##@ Lint

lint: check-purity shellcheck ruff cargo-fmt cargo-clippy clang-format typos check-adr actionlint zizmor ## Everything a pull request must pass

check-purity: ## Mechanical enforcement of PURITY.md
	./scripts/check_purity.sh

shellcheck: ## Every shell script in the tree
	find scripts driver sqlcpu executor rom refemu -name '*.sh' -exec shellcheck {} +

ruff: ## Python, at the version refemu's lockfile pins. ruff.toml holds the rules
	uv run --project refemu ruff check .

cargo-fmt: ## Rust formatting, at the version rust-toolchain.toml pins
	cargo fmt --all --check

cargo-clippy: ## Rust lints. --all-targets so the test files are covered too
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

clang-format: ## C sources. rom/vendor/.clang-format disables the vendored ones
	find rom \( -name '*.c' -o -name '*.h' \) -exec clang-format --dry-run --Werror {} +

typos: ## Spelling, over prose and identifiers. _typos.toml holds the exceptions
	uvx typos@1.50.0 --config _typos.toml

actionlint: ## Workflow syntax. Has no CI job, so run it before pushing a workflow change
	actionlint .github/workflows/*.yml

zizmor: ## Workflow security posture
	zizmor --persona=regular .github/
