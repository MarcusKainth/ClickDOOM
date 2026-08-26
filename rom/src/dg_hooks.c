/* rom/src/dg_hooks.c — doomgeneric's DG_* platform hooks, wired against
 * SPEC §3 MMIO instead of SDL/X11/Win32 (issue #8). Replaces the
 * temporary no-op src/dg_hooks_stub.c (issue #7).
 *
 * No patch to rom/vendor/ was needed for any of this. Build with
 * -DCMAP256 -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200 (rom/Makefile):
 * doomgeneric already has an 8bpp-palette-indexed mode built exactly for
 * platforms like this one (i_video.c, i_video.h) — `pixel_t` becomes
 * `uint8_t`, and DOOM's internal 320x200 buffer (SCREENWIDTH/SCREENHEIGHT,
 * hardcoded in i_video.h, independent of DOOMGENERIC_RESX/RESY) gets
 * copied into `DG_ScreenBuffer` with fb_scaling computed as
 * DOOMGENERIC_RESX/SCREENWIDTH = 320/320 = 1 — no scaling, no offset, a
 * flat 320*200 = 64,000-byte row-major copy. That's exactly SPEC §2's
 * FRAMEBUFFER region, byte for byte, before this file even runs.
 *
 * Palette: doomgeneric's CMAP256 mode exposes `colors[256]` and
 * `palette_changed` as extern globals from i_video.c specifically for a
 * platform to consume (see i_video.h) — no DG_SetPalette hook exists in
 * doomgeneric.h because none is needed. `colors[]` is gamma-corrected
 * (I_SetPalette applies gammatable[usegamma]); SPEC §2 doesn't say
 * anything about gamma, and reading the already-gamma-corrected palette
 * is what every other doomgeneric CMAP256 port does, so that's what this
 * does too — it's "the palette as the game currently displays it," which
 * is also what a real paletted display's hardware would receive.
 */

#include <stdint.h>

#include "doomgeneric.h"
#include "i_video.h"

/* -- SPEC §3 MMIO registers -- */
#define MMIO_BASE 0x10000000u
#define REG_TICKS_MS (*(volatile uint32_t *)(MMIO_BASE + 0x00))
#define REG_KEYQ (*(volatile uint32_t *)(MMIO_BASE + 0x04))
#define REG_FRAME_COMMIT (*(volatile uint32_t *)(MMIO_BASE + 0x10))

/* -- SPEC §2 framebuffer/palette regions --
 *
 * Word stores, not byte stores, for both: SPEC §2's whole rationale for
 * 8bpp-not-32bpp is "4x fewer store instructions per frame on the
 * emulated CPU" -- writing this loop byte-at-a-time would throw half of
 * that win away. Both region sizes are exact multiples of 4
 * (64,000 = 16,000 words; 768 = 192 words), so this covers every byte
 * with no remainder handling.
 */
#define FRAMEBUFFER_BASE 0x11000000u
#define FRAMEBUFFER_SIZE 64000u
#define PALETTE_BASE 0x11010000u
#define PALETTE_SIZE 768u

static volatile uint32_t *const framebuffer_mmio =
    (volatile uint32_t *)FRAMEBUFFER_BASE;
static volatile uint32_t *const palette_mmio =
    (volatile uint32_t *)PALETTE_BASE;

static uint32_t frame_no = 0;

void DG_Init(void) {
  /* Nothing to set up: the framebuffer/palette live at fixed MMIO
   * addresses (no window, no allocation), and DG_ScreenBuffer is
   * already malloc'd by doomgeneric_Create() before this runs. */
}

void DG_DrawFrame(void) {
  if (palette_changed) {
    /* Pack 256 gamma-corrected RGB triples into 768 bytes, then blast
     * them out as 192 word stores rather than 768 byte stores. A
     * local scratch buffer (not itself volatile) lets the compiler
     * do ordinary byte packing here and reserves `volatile` for the
     * MMIO side, which is the only part that actually needs it. */
    uint8_t packed[PALETTE_SIZE];
    for (int i = 0; i < 256; i++) {
      packed[i * 3 + 0] = colors[i].r;
      packed[i * 3 + 1] = colors[i].g;
      packed[i * 3 + 2] = colors[i].b;
    }
    const uint32_t *src = (const uint32_t *)packed;
    for (unsigned i = 0; i < PALETTE_SIZE / 4; i++) {
      palette_mmio[i] = src[i];
    }
    palette_changed = false;
  }

  const uint32_t *src = (const uint32_t *)DG_ScreenBuffer;
  for (unsigned i = 0; i < FRAMEBUFFER_SIZE / 4; i++) {
    framebuffer_mmio[i] = src[i];
  }

  REG_FRAME_COMMIT = frame_no;
  frame_no++;
}

void DG_SleepMs(uint32_t ms) {
  /* No real sleep exists, or should: SPEC §3.1's elastic time means
   * "waiting" is retiring instructions, not blocking on a clock (SPEC
   * §8 forbids wall-clock/host-environment reads on any computation
   * path, and this is squarely on one). Poll TICKS_MS -- itself derived
   * purely from retired-instruction count -- until it has advanced by
   * `ms`. Deterministic, and it costs exactly the instructions elastic
   * time says this wait should cost. */
  uint32_t start = REG_TICKS_MS;
  while (REG_TICKS_MS - start < ms) {
    /* busy-wait */
  }
}

uint32_t DG_GetTicksMs(void) { return REG_TICKS_MS; }

int DG_GetKey(int *pressed, unsigned char *key) {
  uint32_t event = REG_KEYQ;
  if (event == 0) {
    return 0; /* SPEC §3.2: empty queue reads as 0 -- no event */
  }
  *pressed = (int)((event >> 8) & 1u);
  *key = (unsigned char)(event & 0xFFu);
  return 1;
}

void DG_SetWindowTitle(const char *title) {
  (void)title; /* no window here to title */
}
