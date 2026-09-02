# rom/Makefile — reproducible RV32IM ROM build (SPEC §4), driven by the
# pinned toolchain container in toolchain/Dockerfile. `make build-rom` runs
# `make -C rom` with no target, so the first target below (`all`) is the one
# that must produce rom/build/doom-rv32im.bin.
#
# Nothing here assumes a locally installed cross toolchain: every compile
# and link step runs inside the pinned image, mounted at a fixed /work
# regardless of host path.

SHELL := /bin/bash

IMAGE      := clickdoom-rom-toolchain
IMAGE_TAG  := 15.2.0-1
DOCKERFILE := toolchain/Dockerfile
BUILD      := build

# Always the same container *platform*, regardless of what CPU `docker`
# itself is running on (issue #10, found by PINNED_HASH). toolchain/
# Dockerfile picks the right xpack tarball per TARGETARCH -- but "right"
# there meant "matches the host," and the linux-arm64 and linux-x64 xpack
# releases of the *same* toolchain version turned out not to be byte-
# identical: their prebuilt newlib.a each embed a handful of __FILE__-based
# assert() paths from xpack's own build machine (e.g.
# ".../build/linux-arm64/sources/newlib-.../dtoa.c" vs ".../linux-x64/...")
# baked into `libc.a`, which we link. That's a real, if inert (the strings
# are inside `assert()` calls that never fire in this program), difference
# between the two tarballs' compiled output -- 182 bytes, three arm64/amd64
# rebuilds all confirmed it, none of them a fluke. Pinning the *container's*
# platform, not just its image digest, means the ROM is always built by the
# literal same libc.a regardless of which architecture `docker build` is
# invoked from -- under emulation (e.g. QEMU on Apple Silicon) it's slower,
# never wrong. CI runs amd64 natively; this makes every other host match it
# exactly rather than merely "close."
PLATFORM := linux/amd64

BIN      := $(BUILD)/doom-rv32im.bin
ELF      := $(BUILD)/doom-rv32im.elf
MANIFEST := $(BUILD)/manifest.json

# Kept in sync with SPEC.md's own SPEC_VERSION by hand (SPEC.md: "must
# update SPEC_VERSION here and the spec_version constants in code") --
# this is rom's copy of that constant, issue #10.
SPEC_VERSION := 0.1.0

# The real DOOM engine (issue #7), vendored pristine in #41. Excludes:
# other platforms' doomgeneric_*.c ports (this ROM's own hooks are
# src/dg_hooks.c, issue #8) and their audio backends
# (i_{allegro,sdl}{music,sound}.c — no sound device exists here); icon.c
# (X11 window icon, doomgeneric_xlib.c-only); gusconf.c (GUS MIDI config,
# SDL-sound-only); mus2mid.c (unused without FEATURE_SOUND — upstream's
# own default Makefile omits it too, for the same reason).
# Confirmed by actually linking: the resulting undefined-symbol list was
# exactly newlib's syscall surface (syscalls.c) plus mkdir, nothing from a
# missing engine file.
DG_DIR := vendor/doomgeneric/doomgeneric
DG_EXCLUDE := doomgeneric_allegro.c doomgeneric_emscripten.c \
              doomgeneric_linuxvt.c doomgeneric_sdl.c doomgeneric_soso.c \
              doomgeneric_sosox.c doomgeneric_win.c doomgeneric_xlib.c \
              i_allegromusic.c i_allegrosound.c i_sdlmusic.c i_sdlsound.c \
              icon.c gusconf.c mus2mid.c
# The file *list* (which .c files exist) is still read off pristine
# vendor/, never off the patched copy below -- patches/ only ever edits
# the content of files that already exist (rom/patches/README.md), so the
# set of translation units is a property of vendor/ alone. $(sort ...) is
# load-bearing, not tidiness. $(wildcard) order comes from the *host's*
# raw directory-enumeration syscall -- make expands this once, on
# whatever OS invoked `make`, before the file list ever reaches the
# container -- found via issue #10's PINNED_HASH, the first check that
# ever compared a macOS-built ROM against a Linux-built one. macOS
# happened to return these sorted; Linux (every CI runner, and a
# QEMU-emulated Linux container) does not, and source file order changes
# where each translation unit's code/data lands in the linked binary:
# three different hosts (macOS arm64, Linux amd64 under QEMU, GitHub's
# real Linux amd64 runner) produced three different sha256 values, all
# from the identical set of files, before this line had $(sort). $(sort)
# also de-duplicates, but the actual point is making the order a property
# of `make` itself, never of whichever filesystem happened to run it.
DOOM_SRC_NAMES := $(filter-out $(DG_EXCLUDE),$(notdir $(sort $(wildcard $(DG_DIR)/*.c))))

# --- patched vendor copy (rom/patches/, issue #41/#63) ---
# rom/vendor/ is never edited after import (rom/vendor/README.md); any
# deviation this port needs is a patch file in rom/patches/, applied here
# to a build-time copy so vendor/ stays diff-clean against upstream by
# inspection. Compilation reads sources from $(PATCHED_DG_DIR), not
# $(DG_DIR) -- only the file *list* above comes from pristine vendor/, the
# *content* compiled comes from the patched copy, patched or not.
PATCHED_VENDOR := $(BUILD)/vendor
PATCHED_DG_DIR := $(PATCHED_VENDOR)/doomgeneric/doomgeneric
PATCHES        := $(sort $(wildcard patches/*.patch))
DOOM_SRCS      := $(addprefix $(PATCHED_DG_DIR)/,$(DOOM_SRC_NAMES))

# src/dg_hooks.c, src/main.c and src/syscalls.c are the real thing, not
# placeholders — issue #8 retired the last stand-in (src/dg_hooks_stub.c).
# src/wad_embed.S/.c (issue #9) embed rom/wad/doom1.wad (vendored in #60)
# as a rodata blob via .incbin and register it with syscalls.c's virtual
# filesystem before doomgeneric_Create() runs.
SRC := src/crt0.S src/syscalls.c src/main.c src/dg_hooks.c src/wad_embed.S src/wad_embed.c $(DOOM_SRCS)
LD  := toolchain/link.ld
# .incbin's dependency is invisible to make (it's resolved by the
# assembler, not the preprocessor) -- listed explicitly so editing the WAD
# actually triggers a rebuild instead of reusing a stale $(BIN).
WAD := wad/doom1.wad

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
# -DCMAP256 -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200 (issue #8):
# doomgeneric's own 8bpp-palette-indexed platform mode, sized to exactly
# match SCREENWIDTH/SCREENHEIGHT (320x200, hardcoded in i_video.h) so
# doomgeneric's internal scaling computes 1:1 and DG_ScreenBuffer ends up
# byte-identical to SPEC §2's FRAMEBUFFER region with no conversion —
# src/dg_hooks.c just copies it. Without this pair of defines, doomgeneric
# defaults to a 640x400 32bpp RGBA buffer, 16x the store traffic SPEC §2's
# framebuffer choice exists to avoid.
# No -fno-builtin any more: with real libc linked in (below), a builtin
# memcpy/memset substitution resolves to a real symbol instead of an
# undefined one, so there's no reason to forbid it. -ffile-prefix-map
# normalizes the in-container mount path so debug/diagnostic strings never
# vary by host checkout path.
CFLAGS  := -march=rv32im -mabi=ilp32 -mstrict-align -mno-relax \
           -msmall-data-limit=0 -ffreestanding -fno-pic \
           -DCMAP256 -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200 \
           -I$(PATCHED_DG_DIR) -O2 -g0 -fno-ident -ffile-prefix-map=/work=.
# -nostdlib, then -lc -lm -lgcc explicitly: no crt/startfiles pulled in
# implicitly (crt0 is ours — issue #6), but the DOOM engine's libc calls
# (malloc, string.h, printf, ...) are resolved against the toolchain's
# bundled newlib rather than reimplemented by hand. src/syscalls.c (issue
# #7) is the small, well-known syscall surface newlib calls down into —
# _sbrk/_read/_write/_open/... — not a from-scratch libc.
LDFLAGS := -nostdlib -Wl,--build-id=none -T $(LD)
LDLIBS  := -lc -lm -lgcc

# The engine's struct layout for RV32 ILP32, computed by the compiler rather
# than transcribed. toolchain/layout.c is compiled to assembly and never
# linked or objcopied, so it cannot reach $(BIN) and cannot move PINNED_HASH.
LAYOUT     := $(BUILD)/layout.tsv
LAYOUT_ASM := $(BUILD)/layout.s
LAYOUT_SRC := toolchain/layout.c
# refemu's committed copy, which is what the probe reads.
LAYOUT_COMMITTED := ../refemu/probe/layout.tsv

.PHONY: all toolchain-image check-pinned-hash layout check-layout clean

all: $(BIN) $(MANIFEST)

# Build (or reuse the layer cache for) the pinned toolchain image. Every
# input the image depends on is content-pinned in the Dockerfile itself
# (base image by digest, toolchain tarball by verified sha256), so this
# never floats between runs.
toolchain-image: $(DOCKERFILE)
	docker build --platform $(PLATFORM) -t $(IMAGE):$(IMAGE_TAG) -f $(DOCKERFILE) toolchain/

# Copy-then-patch, run on the host (not in the toolchain container): `cp`
# and `patch` are plain deterministic filesystem operations with no
# compiler/toolchain dependence, same footing as check-pinned-hash's
# sha256sum/cat below. One recipe produces the whole patched tree (every
# patch may touch a different file), so this is a stamp file rather than
# a per-source-file rule -- $(BIN) depends on the stamp, not on
# $(DOOM_SRCS) individually, and the stamp's own prerequisites (pristine
# vendor/ sources + patches/*.patch) are what make rebuild propagation
# correct: touch either and the stamp's mtime moves, which moves $(BIN)'s.
# `patch` (not `git apply`): rom/patches/README.md's own stated format.
$(PATCHED_VENDOR)/.stamp: $(sort $(wildcard $(DG_DIR)/*.c $(DG_DIR)/*.h)) $(PATCHES)
	@rm -rf $(PATCHED_VENDOR)
	@mkdir -p $(BUILD)
	cp -r vendor $(PATCHED_VENDOR)
	@for p in $(PATCHES); do \
	  echo "applying $$p"; \
	  patch -p1 -d $(PATCHED_VENDOR) < $$p || exit 1; \
	done
	@touch $@

# Produces both $(BIN) and $(ELF) in one recipe run, but only $(BIN) is a
# tracked Make target -- $(ELF) is always freshly rebuilt alongside it in
# the same invocation, so $(MANIFEST) below only needs to depend on $(BIN)
# for correct ordering. (Deliberately not GNU Make's grouped-target `&:`
# syntax for this: the macOS-shipped `make` is a BSD-frozen 3.81 that
# doesn't understand it and silently double-runs the recipe instead of
# erroring -- portable `:` is worth the one implicit dependency.)
#
# $(SRC) (not individually-listed $(DOOM_SRCS)) is a prerequisite here on
# purpose: the $(DOOM_SRCS) elements live under $(PATCHED_VENDOR), which
# has no per-file rule -- only the .stamp target above knows how to
# produce them, all at once. Depending on $(PATCHED_VENDOR)/.stamp instead
# of $(DOOM_SRCS) gets correct staleness tracking without needing one.
$(BIN): toolchain-image $(PATCHED_VENDOR)/.stamp src/crt0.S src/syscalls.c src/main.c src/dg_hooks.c src/wad_embed.S src/wad_embed.c $(LD) $(WAD)
	@mkdir -p $(BUILD)
	docker run --rm --platform $(PLATFORM) \
	  -v $(CURDIR):/work -w /work \
	  -e SOURCE_DATE_EPOCH=0 -e LC_ALL=C -e TZ=UTC \
	  $(IMAGE):$(IMAGE_TAG) \
	  sh -c 'riscv-none-elf-gcc $(CFLAGS) $(LDFLAGS) -o $(ELF) $(SRC) $(LDLIBS) && \
	         riscv-none-elf-objcopy -O binary --gap-fill=0x00 $(ELF) $(BIN)'

# manifest.json (SPEC §4): emitted from the built ELF/binary, never
# hand-written -- see toolchain/gen_manifest.sh for why entry/load_addr/
# text_start/text_end all come from readelf/nm rather than being
# transcribed constants. Issue #10.
$(MANIFEST): $(BIN) toolchain/gen_manifest.sh
	docker run --rm --platform $(PLATFORM) \
	  -v $(CURDIR):/work -w /work \
	  $(IMAGE):$(IMAGE_TAG) \
	  sh toolchain/gen_manifest.sh $(ELF) $(BIN) $(MANIFEST) $(SPEC_VERSION)

# Not part of `all`: CI runs this as its own step (rom/PINNED_HASH must
# exist first) so a mismatch reports clearly as "reproducibility broke",
# not buried inside a generic build failure. See rom/PINNED_HASH's own
# comment for what a mismatch means and what it never means.
check-pinned-hash: $(BIN)
	@built=$$(sha256sum $(BIN) | cut -d' ' -f1); \
	pinned=$$(cat PINNED_HASH); \
	if [ "$$built" != "$$pinned" ]; then \
	  echo "::error::ROM hash mismatch: built=$$built pinned=$$pinned. Either the build went nondeterministic (P0) or you changed the ROM without updating rom/PINNED_HASH in the same PR." >&2; \
	  exit 1; \
	fi; \
	echo "PINNED_HASH matches: $$built"

# Same compiler, same flags, same patched headers the ROM is built from, so
# the offsets are the ones in the linked binary and not a second opinion.
# -S stops before the assembler: the `@@` lines are what this is for, and the
# object file would only be another thing to delete.
layout: $(LAYOUT)

$(LAYOUT): toolchain-image $(PATCHED_VENDOR)/.stamp $(LAYOUT_SRC)
	@mkdir -p $(BUILD)
	docker run --rm --platform $(PLATFORM) \
	  -v $(CURDIR):/work -w /work \
	  -e SOURCE_DATE_EPOCH=0 -e LC_ALL=C -e TZ=UTC \
	  $(IMAGE):$(IMAGE_TAG) \
	  sh -c 'riscv-none-elf-gcc $(CFLAGS) -S -o $(LAYOUT_ASM) $(LAYOUT_SRC)'
	printf '# clickdoom-layout 1\n# columns\tstruct\tfield\toffset\tsize\n' > $@
	sed -n 's/^[[:space:]]*@@[[:space:]]*//p' $(LAYOUT_ASM) >> $@

# Not part of `all`, for the same reason check-pinned-hash is not: a mismatch
# reports as "the committed layout is stale", not as a build failure.
check-layout: $(LAYOUT)
	@if ! diff -u $(LAYOUT_COMMITTED) $(LAYOUT); then \
	  echo "::error::$(LAYOUT_COMMITTED) does not match the engine's headers. Run: make gen-layout" >&2; \
	  exit 1; \
	fi; \
	echo "layout.tsv matches: $$(grep -c -v '^#' $(LAYOUT)) rows"

clean:
	rm -rf $(BUILD)
