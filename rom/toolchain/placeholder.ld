/* Placeholder linker script for the toolchain smoke test (issue #5).
 *
 * Deliberately not the real memory map: SPEC §2's text/rodata/data/bss/heap/
 * stack layout and the read-only text region ADR-0002 depends on land in
 * issue #6, which replaces this file. This script only places the
 * placeholder entry point at the SPEC §1 reset address so the flat binary
 * objcopy produces is loadable at 0x8000_0000.
 */

ENTRY(_start)

MEMORY
{
    RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 24M
}

SECTIONS
{
    . = 0x80000000;

    .text : { *(.text*) } > RAM

    /* Explicitly drop metadata sections rather than let the default orphan
     * rules decide where they land — objcopy -O binary would otherwise
     * happily include any of these if a future toolchain marks them
     * allocatable, which would break byte-reproducibility in a way that's
     * hard to notice. */
    /DISCARD/ : { *(.comment) *(.note.*) *(.riscv.attributes) }
}
