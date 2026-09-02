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
CH_HTTP_PORT ?= 8123

conn = --host $(CH_HOST) --port $(CH_PORT) --password "$(CLICKHOUSE_PASSWORD)"

# The clickdoom binary speaks ClickHouse's HTTP interface, not the native
# protocol scripts/clickhouse-client use, so it takes its own port.
CLICKDOOM ?= ./target/release/clickdoom
clickdoom_conn = --host $(CH_HOST) --port $(CH_HTTP_PORT) --password "$(CLICKHOUSE_PASSWORD)" --database "$(CLICKDOOM_DATABASE)"

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
        test smoke diff \
        bench-canonical-throughput \
        preflight-milestone run-milestone \
        build-refemu build-clickdoom build-riscv-tests-fixtures gen-reference-trace gen-demo3-trace \
        fuzz \
        lint check-purity shellcheck format clippy typos actionlint zizmor \
        adr-new check-adr require-rom \
        gates check-rom-hash

help: ## List every target
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage: make <target>\n"} \
		/^[a-zA-Z0-9_-]+:.*?##/ { printf "  \033[36m%-28s\033[0m %s\n", $$1, $$2 } \
		/^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) }' $(MAKEFILE_LIST)
	@echo

##@ Environment

up: ## Start the pinned ClickHouse container
	# A server already answering is left alone, so `make test` behaves the
	# same against a local compose container and against CI's service
	# container, which binds these ports before make ever runs.
	curl -fsS "http://$(CH_HOST):$(CH_HTTP_PORT)/ping" >/dev/null 2>&1 \
	    || docker compose up -d --wait

down: ## Stop it
	docker compose down

##@ Build

build-rom: ## Build the DOOM ROM reproducibly, in the pinned toolchain image
	make -C rom

build-refemu: ## Build the reference emulator
	cargo build --locked --release -p refemu

build-clickdoom: ## Build the driver binary
	cargo build --locked --release -p clickdoom-driver

require-rom:
	test -f $(ROM_BIN) || { echo "$(ROM_BIN) missing. Run: make build-rom" >&2; exit 1; }

##@ Test

# Two invocations, not one: the reference-trace and demo3 comparisons need
# --release to finish in reasonable time, and everything else runs in debug.
# The live suites need the container `up` starts; the rest do not.
test: up require-rom build-refemu ## Every suite, live ones included
	CLICKHOUSE_HOST=$(CH_HOST) CLICKHOUSE_HTTP_PORT=$(CH_HTTP_PORT) CLICKHOUSE_PASSWORD="$(CLICKHOUSE_PASSWORD)" \
	    cargo test --locked --workspace --features clickhouse-tests -- --nocapture
	cargo test --locked --release --workspace --features refemu/rom-tests \
	    --test reference_trace --test demo3_parity --test rom_symbols -- --nocapture

N ?= 100000
diff: up require-rom build-refemu build-clickdoom ## Differential run of N instructions, reporting the first divergence
	$(CLICKDOOM) emulation diff $(N) --bin $(ROM_BIN) --manifest $(ROM_MANIFEST) \
		--hwm "$(CLICKDOOM_RUN_HWM)" --refemu-bin $(REFEMU) $(clickdoom_conn)

smoke: ## The differential run CI uses, at 100,000 instructions
	$(MAKE) diff N=100000

##@ Bench
#
# Timings need a quiet machine. docs/benchmarks.md indexes what has already
# been measured, and DEVELOPING.md says what to record alongside a number.

# The image each bench arm starts its own container from, read out of
# docker-compose.yml so the pin is stated in one place.
clickhouse_image = $(shell sed -n 's|^ *image: \(clickhouse/clickhouse-server.*\)$$|\1|p' docker-compose.yml)

# No `up`: each arm starts and removes a container of its own, so this target
# does not touch the shared one.
bench-canonical-throughput: require-rom build-refemu build-clickdoom ## Real-ROM throughput: boot and gameplay windows, fold-alone and end to end
	$(CLICKDOOM) emulation bench canonical --bin $(ROM_BIN) --manifest $(ROM_MANIFEST) \
		--image "$(clickhouse_image)" \
		--k "$(CLICKDOOM_RUN_K)" --hwm "$(CLICKDOOM_RUN_HWM)" \
		--refemu-bin $(REFEMU)

##@ Milestone

preflight-milestone: up require-rom build-clickdoom ## Fail-closed gates before a multi-hour run. Refuses to start rather than advising
	$(CLICKDOOM) emulation preflight --bin "$(ROM_BIN)" --manifest "$(ROM_MANIFEST)" \
		--k "$(CLICKDOOM_RUN_K)" --hwm "$(CLICKDOOM_RUN_HWM)" \
		$(clickdoom_conn)

run-milestone: up require-rom build-clickdoom ## The resumable batch loop. Runs its own preflight and refuses to start if it fails
	$(CLICKDOOM) emulation run --bin "$(ROM_BIN)" --manifest "$(ROM_MANIFEST)" \
		--k "$(CLICKDOOM_RUN_K)" --hwm "$(CLICKDOOM_RUN_HWM)" \
		--trace "$(reference_trace)" \
		--target-icount "$(CLICKDOOM_TARGET_ICOUNT)" \
		--stop-at-frame 0 \
		$(clickdoom_conn)

##@ Maintenance

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

lint: check-purity shellcheck format clippy typos check-adr actionlint zizmor ## Every static check. No container, no ROM

check-purity: ## Mechanical enforcement of PURITY.md
	./scripts/check_purity.sh

shellcheck: ## Every shell script in the tree
	git ls-files '*.sh' | xargs shellcheck

format: ## Formatting, every language. Rust at rust-toolchain.toml's version
	cargo fmt --all --check
	find rom \( -name '*.c' -o -name '*.h' \) -exec clang-format --dry-run --Werror {} +

clippy: ## Rust lints. --all-targets so the test files are covered too
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

typos: ## Spelling, over prose and identifiers. _typos.toml holds the exceptions
	cargo install --locked --quiet typos-cli@1.50.0
	typos --config _typos.toml

actionlint: ## Workflow syntax. Has no CI job, so run it before pushing a workflow change
	actionlint .github/workflows/*.yml

zizmor: ## Workflow security posture
	cargo install --locked --quiet zizmor@1.28.0
	zizmor --persona=regular .github/

##@ Gates
#
# Prerequisites in cost order. `lint` needs no container and no ROM, and
# `check-rom-hash` builds the ROM that `test` and `smoke` both require. Make stops at the first one that fails.
#
# `check-rom-hash` names both goals in one `make -C rom` rather than depending
# on `build-rom` and recursing twice. rom/Makefile's binary depends on the
# phony `toolchain-image`, so it is rebuilt on every entry, and two entries
# compile the ROM twice.

gates: lint check-rom-hash test smoke ## Every check ci.yml runs on a pull request

check-rom-hash: ## The built ROM matches rom/PINNED_HASH
	make -C rom all check-pinned-hash
