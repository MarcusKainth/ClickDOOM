# rom/patches/ — every deviation from pristine upstream

Issue #41. `rom/vendor/` is never edited after import (see
`rom/vendor/README.md`); anything the ClickDOOM port needs that upstream
doomgeneric doesn't already provide — the `DG_*` hooks wired to SPEC §3
MMIO instead of SDL/X11/Win32 (#8), libc call sites that need our shims
instead of a real libc (#7), any RV32IM-bare-metal-specific fix — becomes a
patch file here, applied to a working copy of `rom/vendor/` at build time.
Never hand-edit files under `rom/vendor/` to work around something a patch
should do instead; that's exactly the shortcut this split exists to make
impossible to take by accident.

One patch file per logical change, named for what it does
(`0001-short-description.patch`), in the format `git diff` /
`git format-patch` produces against the pinned vendor commit, so each is
individually reviewable and `patch -p1`/`git apply` can apply it.

Empty for now. Issue #7/#8 land the first patches, and the same PR that
adds one is the PR that wires `rom/Makefile` to apply everything here to a
build-time copy of `rom/vendor/doomgeneric/` before compiling (deliberately
not wired up yet — see #41's PR discussion; this directory existing is
what #7/#8/#9 needed to stop being blocked, the Makefile step lands
alongside the first real patch).
