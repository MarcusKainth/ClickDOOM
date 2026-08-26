/* rom/toolchain/link.ld — SPEC §2 memory map.
 *
 * Places .text as its own contiguous, word-aligned region so it can be
 * delimited by text_start/text_end in manifest.json (SPEC §4) and
 * pre-decoded by the executor (ADR-0002) — __text_start/__text_end below
 * are exactly those bounds, read by issue #10's manifest emitter rather
 * than written by hand. Nothing writable is ever placed inside that
 * region: .rodata, .data, .bss, the heap and the stack all live after it.
 * A store into [__text_start, __text_end) is a SELF_MODIFY fatal halt
 * (SPEC §1); this layout is what makes that boundary well-defined.
 */

ENTRY(_start)

MEMORY
{
    RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 24M
}

/* Not SPEC-mandated — a rom/-local implementation choice, revisit if
 * doomgeneric turns out to need more. */
STACK_SIZE = 1M;

/* Separate PT_LOAD segments with distinct, correct permissions per region
 * (R+E for text, R for rodata, R+W for data/bss) instead of the linker's
 * default single RWX segment. This has no effect on the flat binary
 * objcopy produces — the SQL CPU has no MMU/page protection (SPEC §1); the
 * text/writable boundary is enforced by address-range checking against
 * __text_start/__text_end, not by these flags. It's ELF hygiene: it's what
 * makes `readelf -l` tell the truth, and it silences the (correct) linker
 * warning about a combined RWX segment. */
PHDRS
{
    text   PT_LOAD FLAGS(5); /* R+E */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* R+W */
}

SECTIONS
{
    . = ORIGIN(RAM);

    /* --- text: code only, nothing writable, exactly delimited --- */
    __text_start = .;
    .text : {
        *(.text.crt0)          /* _start first: pc = 0x8000_0000 must land here */
        *(.text .text.*)
    } > RAM :text
    . = ALIGN(4);               /* RV32IM instructions are always word-aligned */
    __text_end = .;

    /* --- everything else: ordinary read/write RAM ---
     *
     * .s{data,bss,rodata}* are explicitly folded into their plain
     * counterparts rather than left as orphans: rom/Makefile passes
     * -msmall-data-limit=0 so gcc shouldn't emit them at all, but that's a
     * compile flag on objects we control — a future .o built with different
     * flags (or by a different toolchain) could still produce one, and an
     * orphan .sbss earlier silently escaped __bss_start/__bss_end and never
     * got zeroed by crt0. Covering the small-data names here means that
     * class of bug can't recur even if the flag lapses somewhere. */
    .rodata : {
        *(.rodata .rodata.* .srodata .srodata.*)
    } > RAM :rodata
    . = ALIGN(4);

    .data : {
        __data_start = .;
        *(.data .data.* .sdata .sdata.*)
        __data_end = .;
    } > RAM :data
    . = ALIGN(4);

    /* NOLOAD: .bss occupies address space but contributes no file bytes —
     * objcopy -O binary correctly stops emitting bytes at end of .data.
     * The executor's RAM starts zeroed, so nothing needs to zero-fill this
     * region for us; crt0 zeroes it anyway because SPEC §1 requires it of
     * the ROM itself, not because either engine happens to start zeroed. */
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.* .sbss .sbss.* COMMON)
        __bss_end = .;
    } > RAM :data
    . = ALIGN(4);

    __heap_start = .;
    __stack_top = ORIGIN(RAM) + LENGTH(RAM);
    __stack_bottom = __stack_top - STACK_SIZE;
    __heap_end = __stack_bottom;

    ASSERT(__heap_start < __heap_end, "heap region is empty or negative: RAM too small for STACK_SIZE")
    ASSERT(__text_end > __text_start, "text region is empty")
    ASSERT(__stack_top <= ORIGIN(RAM) + LENGTH(RAM), "stack overflows the 24 MiB RAM window")

    /* .init_array/.fini_array: crtbegin.o registers register_fini here
     * (C++/atexit-style destructor bookkeeping, pulled in by -lgcc even
     * for a pure-C link) once the real DOOM engine got linked in (#7).
     * crt0 goes straight from zeroing .bss to `call main` and never walks
     * this array -- correct for a program that only ever stops via SPEC
     * §3's EXIT register, not a graceful C-runtime shutdown. Discarding
     * it here rather than silently leaving it unprocessed in .data is the
     * honest version of the same decision. */
    /DISCARD/ : {
        *(.comment) *(.note.*) *(.riscv.attributes) *(.eh_frame) *(.eh_frame_hdr)
        *(.init_array*) *(.fini_array*)
    }
}
