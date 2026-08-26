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
 * argv (issue #107): README's Definition of Victory is `doom -timedemo
 * demo3` running to completion. Without a command line, `M_CheckParm`
 * can never find `-timedemo`, DOOM falls into its attract-mode demo loop
 * instead (which never terminates -- no `EXIT` write, no final frame to
 * hash), and the ROM can never actually run the thing README promises.
 * `doom_argv` below is the whole answer: fixed, compiled in, no build-time
 * switch. Deliberately not configurable -- a switch would need
 * manifest.json (SPEC §4) to say which argv an artifact carries, which is
 * a SPEC-governed schema change for what should just be "the one ROM this
 * repo ships." A boot-to-attract-mode build stays useful for debugging,
 * but that is a separate, unpinned, explicitly-deferred convenience
 * artifact (see the follow-up issue), not a second version of this one.
 *
 * `doom_argv`'s entries are mutable `static char[]`, not string literals
 * aliased through a non-const `char *` -- nothing in doomgeneric writes
 * through `argv` today, but a real OS-supplied argv is ordinary writable
 * memory and there is no reason to leave that question open when being
 * explicit costs nothing.
 *
 * `M_CheckParmWithArgs`'s own loop bound is `i < myargc - num_args`
 * (rom/vendor/doomgeneric/doomgeneric/m_argv.c) -- it needs argc to be
 * *exactly* right (3: program name, `-timedemo`, `demo3`), not just
 * "arguments present", or the scan silently never reaches `-timedemo` at
 * all. `D_AddFile("demo3.lmp")` (d_main.c) will fail -- there is no such
 * file in syscalls.c's WAD-only VFS -- but DOOM's own code already
 * handles that: on failure it falls back to treating "demo3" itself as
 * the lump name ("makes tricks like -playdemo demo1 possible", d_main.c),
 * which resolves against `doom1.wad`'s built-in `DEMO3` lump. No VFS
 * change needed. Both of these were verified by actually building and
 * running this exact change through refemu, not by reading the arithmetic
 * -- see issue #107.
 *
 * wad_embed_register() (issue #9) must run before doomgeneric_Create():
 * D_DoomMain's IWAD search happens synchronously inside it. See
 * wad_embed.h for why this is an explicit call rather than a constructor.
 */

#include "doomgeneric.h"
#include "wad_embed.h"

static char doom_argv0[] = "doom";
static char doom_argv1[] = "-timedemo";
static char doom_argv2[] = "demo3";
static char *doom_argv[] = {doom_argv0, doom_argv1, doom_argv2};

int main(void) {
  wad_embed_register();
  doomgeneric_Create(3, doom_argv);

  for (;;) {
    doomgeneric_Tick();
  }

  return 0;
}
