# rom/

See CLAUDE.md for this workstream's charter and SPEC.md for its contracts.
Ownership is claimed via issue self-assignment.

## Build

    just build-rom

produces `rom/build/doom-rv32im.bin` (and `.elf`). Nothing beyond Docker is
required on the host — `rom/Makefile` builds a pinned toolchain container
(`rom/toolchain/Dockerfile`) and runs every compile/link step inside it, so
the build is byte-reproducible on any host (verified: identical sha256
across repeat builds and after evicting the local image cache).

## Toolchain (issue #5)

**xPack GNU RISC-V Embedded GCC v15.2.0-1** (`riscv-none-elf-*`), a
bare-metal ("no known OS") target — matches the charter, since rom/ brings
its own crt0 (#6) and libc shims (#7) rather than linking newlib against an
OS that doesn't exist here. Fetched in the Dockerfile from the upstream
GitHub release and verified against a sha256 pinned from the Releases API
asset digest (independent of xpack's own `.sha` file). Base image
(`debian:bookworm-slim`) is pinned by manifest-list digest, not a floating
tag.

Both are content-pinned, so a bump is a deliberate `ci:`/`rom:` PR that
changes the pin, never something that happens by re-pulling `latest`.

## Placeholder entry point

`src/placeholder.S` and `toolchain/placeholder.ld` are **not** the real ROM.
They exist only so this Makefile has something real to compile, activating
CI's `build-rom` job before crt0 and the real linker script/memory map land
in #6. Both files say so in their header comments and are meant to be
deleted/replaced there, not extended.
