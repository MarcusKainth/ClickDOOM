#!/usr/bin/env sh
# rom/toolchain/gen_manifest.sh — emits rom/build/manifest.json (SPEC §4).
#
# Run inside the pinned toolchain container by rom/Makefile, immediately
# after the ELF/flat-binary build, so every field is read from the actual
# build artifacts rather than hand-transcribed. `entry`, `load_addr`,
# `text_start` and `text_end` all come from the linked ELF itself
# (readelf/nm) -- in particular, __text_start/__text_end are the same
# toolchain/link.ld symbols that delimit the SELF_MODIFY-protected region
# (SPEC §1/§2, ADR-0002). A hand-copied bound here could silently drift
# from the linker script and disable that protection without anyone
# noticing; reading it from the build every time is what SPEC §4 means by
# "the build emits them ... rather than having them written by hand."
set -eu

ELF="$1"
BIN="$2"
OUT="$3"
SPEC_VERSION="$4"

# nm/readelf hex values show up with or without a leading 0x depending on
# the tool; normalize before handing them to the shell's arithmetic parser.
hex_to_dec() {
    case "$1" in
        0x* | 0X*) printf '%d\n' "$1" ;;
        *) printf '%d\n' "0x$1" ;;
    esac
}

entry_hex=$(riscv-none-elf-readelf -h "$ELF" | awk '/Entry point address/ { print $NF }')
load_addr_hex=$(riscv-none-elf-readelf -l "$ELF" | awk '/^ *LOAD/ { print $3; exit }')
text_start_hex=$(riscv-none-elf-nm "$ELF" | awk '$3 == "__text_start" { print $1 }')
text_end_hex=$(riscv-none-elf-nm "$ELF" | awk '$3 == "__text_end" { print $1 }')

for name in entry_hex load_addr_hex text_start_hex text_end_hex; do
    eval "val=\${$name}"
    if [ -z "$val" ]; then
        echo "gen_manifest.sh: could not find $name in $ELF -- toolchain/link.ld or crt0.S may have changed shape" >&2
        exit 1
    fi
done

entry=$(hex_to_dec "$entry_hex")
load_addr=$(hex_to_dec "$load_addr_hex")
text_start=$(hex_to_dec "$text_start_hex")
text_end=$(hex_to_dec "$text_end_hex")
size=$(wc -c <"$BIN" | tr -d ' ')
sha256=$(sha256sum "$BIN" | cut -d' ' -f1)

cat >"$OUT" <<JSON
{
  "spec_version": "$SPEC_VERSION",
  "entry": $entry,
  "load_addr": $load_addr,
  "size": $size,
  "sha256": "$sha256",
  "text_start": $text_start,
  "text_end": $text_end
}
JSON
