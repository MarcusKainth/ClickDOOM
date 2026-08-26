# rom/src/wad_embed.S — embeds the shareware WAD as a rodata blob (issue
# #9). `.incbin`'s path is resolved relative to the assembler's working
# directory, which rom/Makefile always sets to /work inside the toolchain
# container (the mounted rom/ tree) -- so "wad/doom1.wad" here means
# rom/wad/doom1.wad, matching how every other path in this build is
# already container-relative.
#
# `.rodata.wad` (not plain `.rodata`) only so this blob is trivially
# identifiable in a section listing; toolchain/link.ld's `.rodata` output
# section already globs `.rodata.*`, so it lands in the same read-only
# region as everything else -- no linker script change needed.

    .section .rodata.wad
    .global _wad_doom1_start
    .global _wad_doom1_end
_wad_doom1_start:
    .incbin "wad/doom1.wad"
_wad_doom1_end:
