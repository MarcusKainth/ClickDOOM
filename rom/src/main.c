/* rom/src/main.c — the ROM's real entry point (called by crt0 after sp is
 * set and .bss is zeroed).
 *
 * doomgeneric's own documented usage pattern (rom/vendor/doomgeneric/
 * README.md, "main loop"): call doomgeneric_Create() once, then call
 * doomgeneric_Tick() in a loop. Both steps are load-bearing --
 * doomgeneric's own D_DoomLoop() (d_main.c) runs *one* tic's worth of
 * one-time setup plus doomgeneric_Tick() and then returns; it is the
 * platform's job to call doomgeneric_Tick() again for every tic after
 * that. An earlier version of this file only called doomgeneric_Create()
 * on the (wrong) assumption that D_DoomLoop never returns -- true of
 * vanilla DOOM's D_DoomLoop, not of doomgeneric's redesigned one. That
 * bug was invisible until issue #9 gave the engine a WAD to actually run
 * D_DoomLoop with: without one, D_DoomMain halts at the IWAD search
 * (issue #7's finding) long before reaching D_DoomLoop, so the missing
 * doomgeneric_Tick() loop never got exercised. Caught by booting this ELF
 * in refemu and finding pc parked at *this file's own* `for(;;)` after a
 * single FRAME_COMMIT instead of climbing past it.
 *
 * Not a placeholder: unlike src/dg_hooks_stub.c (now retired), this is
 * what the real ROM does. argc/argv are 0/NULL -- there is no command
 * line here.
 *
 * wad_embed_register() (issue #9) must run before doomgeneric_Create():
 * D_DoomMain's IWAD search happens synchronously inside it. See
 * wad_embed.h for why this is an explicit call rather than a constructor.
 */

#include "doomgeneric.h"
#include "wad_embed.h"

int main(void) {
  wad_embed_register();
  doomgeneric_Create(0, 0);

  for (;;) {
    doomgeneric_Tick();
  }

  return 0;
}
