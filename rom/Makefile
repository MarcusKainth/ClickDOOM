# rom/Makefile — reproducible RV32IM ROM build (SPEC §4), driven by the
# pinned toolchain container in toolchain/Dockerfile. `just build-rom` runs
# `make -C rom` with no target, so the first target below (`all`) is the one
# that must produce rom/build/doom-rv32im.bin.
#
# Nothing here assumes a locally installed cross toolchain: every compile
# and link step runs inside the pinned image, mounted at a fixed /work
# regardless of host path, so the build reproduces byte-for-byte on any host
# with Docker, including CI.

SHELL := /bin/bash

IMAGE      := clickdoom-rom-toolchain
IMAGE_TAG  := 15.2.0-1
DOCKERFILE := toolchain/Dockerfile
BUILD      := build

BIN := $(BUILD)/doom-rv32im.bin
ELF := $(BUILD)/doom-rv32im.elf

# Sources for the current milestone. main_stub.c stands in for doomgeneric's
# main (issues #7-#9 supply libc and the real DG_* hooks; see the header
# comment in that file) — it exists so crt0's `call main` has a real symbol
# and the build keeps producing something real to run.
SRC := src/crt0.S src/main_stub.c
LD  := toolchain/link.ld

# SPEC §1: RV32IM, no compressed extension, strict alignment (keeps the SQL
# load/store path branch-free). -mno-relax: never emit gp-relative linker
# relaxation (see src/crt0.S — this is why crt0 sets no gp).
# -msmall-data-limit=0: gcc's RISC-V backend routes small globals into
# .sdata/.sbss/.srodata below a size threshold *regardless* of -mno-relax
# (that flag only suppresses the gp-relative addressing transform, not the
# section split) — found the hard way when a .sbss orphan landed outside
# toolchain/link.ld's .bss output section and crt0's zero loop silently
# skipped it. Zeroing the threshold means everything lands in the plain
# .data/.bss/.rodata this linker script actually accounts for.
# -fno-builtin: freestanding means no libc, so gcc must never substitute a
# builtin call (memset/memcpy/...) for a recognized loop pattern — there is
# nothing behind that symbol yet. -ffile-prefix-map normalizes the
# in-container mount path so debug/diagnostic strings never vary by host
# checkout path.
CFLAGS  := -march=rv32im -mabi=ilp32 -mstrict-align -mno-relax \
           -msmall-data-limit=0 -ffreestanding -fno-pic -fno-builtin \
           -O2 -g0 -fno-ident -ffile-prefix-map=/work=.
# -nostdlib: no crt/libc/libgcc linked in. There is no OS here (rom/
# charter); crt0 and libc shims are ours (issues #6, #7).
LDFLAGS := -nostdlib -Wl,--build-id=none -T $(LD)

.PHONY: all toolchain-image clean

all: $(BIN)

# Build (or reuse the layer cache for) the pinned toolchain image. Every
# input the image depends on is content-pinned in the Dockerfile itself
# (base image by digest, toolchain tarball by verified sha256), so this
# never floats between runs.
toolchain-image: $(DOCKERFILE)
	docker build -t $(IMAGE):$(IMAGE_TAG) -f $(DOCKERFILE) toolchain/

$(BIN): toolchain-image $(SRC) $(LD)
	@mkdir -p $(BUILD)
	docker run --rm \
	  -v $(CURDIR):/work -w /work \
	  -e SOURCE_DATE_EPOCH=0 -e LC_ALL=C -e TZ=UTC \
	  $(IMAGE):$(IMAGE_TAG) \
	  sh -c 'riscv-none-elf-gcc $(CFLAGS) $(LDFLAGS) -o $(ELF) $(SRC) && \
	         riscv-none-elf-objcopy -O binary --gap-fill=0x00 $(ELF) $(BIN)'

clean:
	rm -rf $(BUILD)
