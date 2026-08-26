/* rom/src/main.c — the ROM's real entry point (called by crt0 after sp is
 * set and .bss is zeroed).
 *
 * This is doomgeneric's own documented usage pattern (rom/vendor/doomgeneric/
 * README.md, "main loop"): call doomgeneric_Create() once. It runs
 * D_DoomMain(), which ends in D_DoomLoop(), which never returns -- our own
 * for(;;) below only matters if that invariant is ever violated (crt0
 * would otherwise fall through into whatever bytes follow _start in RAM).
 *
 * Not a placeholder: unlike src/dg_hooks_stub.c, this is what the real ROM
 * does. argc/argv are 0/NULL -- there is no command line here.
 */

#include "doomgeneric.h"

int main(void) {
  doomgeneric_Create(0, 0);

  for (;;) {
    /* unreachable in practice; see comment above */
  }

  return 0;
}
