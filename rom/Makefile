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

# The real DOOM engine (issue #7), vendored pristine in #41. Excludes:
# other platforms' doomgeneric_*.c ports (this ROM's own hooks are
# src/dg_hooks_stub.c for now, the real ones land in #8) and their
# audio backends (i_{allegro,sdl}{music,sound}.c — no sound device exists
# here); icon.c (X11 window icon, doomgeneric_xlib.c-only); gusconf.c (GUS
# MIDI config, SDL-sound-only); mus2mid.c (unused without FEATURE_SOUND —
# upstream's own default Makefile omits it too, for the same reason).
# Confirmed by actually linking: the resulting undefined-symbol list was
# exactly newlib's syscall surface (syscalls.c) plus mkdir, nothing from a
# missing engine file.
DG_DIR := vendor/doomgeneric/doomgeneric
DG_EXCLUDE := doomgeneric_allegro.c doomgeneric_emscripten.c \
              doomgeneric_linuxvt.c doomgeneric_sdl.c doomgeneric_soso.c \
              doomgeneric_sosox.c doomgeneric_win.c doomgeneric_xlib.c \
              i_allegromusic.c i_allegrosound.c i_sdlmusic.c i_sdlsound.c \
              icon.c gusconf.c mus2mid.c
DOOM_SRCS := $(filter-out $(addprefix $(DG_DIR)/,$(DG_EXCLUDE)),$(wildcard $(DG_DIR)/*.c))

# src/dg_hooks_stub.c is a TEMPORARY no-op stand-in for the real SPEC §3
# MMIO-backed DG_* hooks (issue #8 replaces it, not extends it — see the
# header comment in that file). src/main.c and src/syscalls.c are the real
# thing, not placeholders.
SRC := src/crt0.S src/syscalls.c src/main.c src/dg_hooks_stub.c $(DOOM_SRCS)
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
# No -fno-builtin any more: with real libc linked in (below), a builtin
# memcpy/memset substitution resolves to a real symbol instead of an
# undefined one, so there's no reason to forbid it. -ffile-prefix-map
# normalizes the in-container mount path so debug/diagnostic strings never
# vary by host checkout path.
CFLAGS  := -march=rv32im -mabi=ilp32 -mstrict-align -mno-relax \
           -msmall-data-limit=0 -ffreestanding -fno-pic \
           -I$(DG_DIR) -O2 -g0 -fno-ident -ffile-prefix-map=/work=.
# -nostdlib, then -lc -lm -lgcc explicitly: no crt/startfiles pulled in
# implicitly (crt0 is ours — issue #6), but the DOOM engine's libc calls
# (malloc, string.h, printf, ...) are resolved against the toolchain's
# bundled newlib rather than reimplemented by hand. src/syscalls.c (issue
# #7) is the small, well-known syscall surface newlib calls down into —
# _sbrk/_read/_write/_open/... — not a from-scratch libc.
LDFLAGS := -nostdlib -Wl,--build-id=none -T $(LD)
LDLIBS  := -lc -lm -lgcc

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
	  sh -c 'riscv-none-elf-gcc $(CFLAGS) $(LDFLAGS) -o $(ELF) $(SRC) $(LDLIBS) && \
	         riscv-none-elf-objcopy -O binary --gap-fill=0x00 $(ELF) $(BIN)'

clean:
	rm -rf $(BUILD)
