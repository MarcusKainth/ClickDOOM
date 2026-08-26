#!/usr/bin/env bash
# Regenerate refemu/tests/fixtures/riscv_tests/*.bin from upstream
# riscv-tests sources, built against ClickDOOM's minimal environment
# (riscv-tests-env/, see riscv_test.h for why upstream's env/p won't work
# here). This is a maintenance script, not part of `just test-refemu` --
# the fixtures it produces are committed, deterministic, tiny (a few KB
# each), and CI never needs a RISC-V toolchain to run the test suite.
# Re-run this only when bumping RISCV_TESTS_REV or fixing the environment.
#
# Requires an RV32IM-capable toolchain on PATH. Tested with Homebrew's
# `riscv64-elf-gcc` (GCC 16.2.0 / binutils 2.47), which supports the
# rv32im/ilp32 multilib directly -- set RVGCC/RVOBJCOPY to override.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REFEMU_DIR="$(dirname "$SCRIPT_DIR")"
ENV_DIR="$REFEMU_DIR/riscv-tests-env"
FIXTURES_DIR="$REFEMU_DIR/tests/fixtures/riscv_tests"

RVGCC="${RVGCC:-riscv64-elf-gcc}"
RVOBJCOPY="${RVOBJCOPY:-riscv64-elf-objcopy}"

# Pinned so fixture regeneration is reproducible; bump deliberately.
RISCV_TESTS_REPO="https://github.com/riscv-software-src/riscv-tests.git"
RISCV_TESTS_REV="2ebecad997fa58cd9e5724340ba75aa4b59bd1d0"
RISCV_TESTS_DIR="${RISCV_TESTS_DIR:-}"

if [ -z "$RISCV_TESTS_DIR" ]; then
  RISCV_TESTS_DIR="$(mktemp -d)/riscv-tests"
  echo "Cloning riscv-tests @ $RISCV_TESTS_REV into $RISCV_TESTS_DIR"
  git clone --quiet "$RISCV_TESTS_REPO" "$RISCV_TESTS_DIR"
  git -C "$RISCV_TESTS_DIR" checkout --quiet "$RISCV_TESTS_REV"
fi

ISA_DIR="$RISCV_TESTS_DIR/isa"
MACROS_DIR="$ISA_DIR/macros/scalar"

# rv32ui, minus two tests that assume hardware ClickDOOM's CPU model does
# not have (SPEC §1 / ADR-0002 -- these are deliberate exclusions, not
# oversights):
#   fence_i -- tests self-modifying code + fence.i; SPEC §1/ADR-0002 make
#              a store into the text region a fatal SELF_MODIFY halt.
#   ma_data -- tests that misaligned loads/stores transparently succeed;
#              SPEC §1 makes misaligned word/halfword access a fatal halt.
RV32UI_TESTS="add addi and andi auipc beq bge bgeu blt bltu bne jal jalr lb lbu ld_st lh lhu lui lw or ori sb sh simple sll slli slt slti sltiu sltu sra srai srl srli st_ld sub sw xor xori"
RV32UM_TESTS="div divu mul mulh mulhsu mulhu rem remu"

mkdir -p "$FIXTURES_DIR"
rm -f "$FIXTURES_DIR"/*.bin

build_one() {
  local suite="$1" name="$2"
  local src="$ISA_DIR/$suite/$name.S"
  local elf out_name
  elf="$(mktemp)"
  out_name="${suite}-p-${name}"
  "$RVGCC" -march=rv32im -mabi=ilp32 -static -mcmodel=medany \
    -nostdlib -nostartfiles -fno-builtin \
    -I "$ENV_DIR" -I "$MACROS_DIR" \
    -T "$ENV_DIR/link.ld" \
    -o "$elf" "$src"
  "$RVOBJCOPY" -O binary "$elf" "$FIXTURES_DIR/$out_name.bin"
  rm -f "$elf"
  echo "  built $out_name.bin"
}

echo "Building rv32ui fixtures..."
for name in $RV32UI_TESTS; do build_one rv32ui "$name"; done

echo "Building rv32um fixtures..."
for name in $RV32UM_TESTS; do build_one rv32um "$name"; done

count=$(find "$FIXTURES_DIR" -name '*.bin' | wc -l | tr -d ' ')
echo "Done: $count fixtures in $FIXTURES_DIR"
