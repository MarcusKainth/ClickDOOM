/* Linker script for building upstream riscv-tests sources against
 * ClickDOOM's minimal environment (riscv_test.h in this directory).
 * Places .text.init at SPEC §2's RAM base -- refemu's reset state is
 * pc = 0x8000_0000, so the test binary's entry point has to be there too.
 * No .tohost section: this environment doesn't use the tohost/fromhost
 * mechanism (see riscv_test.h) and nothing in rv32ui/rv32um references it.
 */
OUTPUT_ARCH("riscv")
ENTRY(_start)

SECTIONS
{
  . = 0x80000000;
  .text.init : { *(.text.init) }
  .text : { *(.text) }
  . = ALIGN(4);
  .data : { *(.data) }
  .bss : { *(.bss) }
  _end = .;
}
