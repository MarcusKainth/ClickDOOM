/* rom/src/dg_hooks_stub.c — TEMPORARY placeholder for doomgeneric's DG_*
 * platform hooks (issue #8), which wire these against SPEC §3 MMIO
 * (TICKS_MS, KEYQ, FRAME_COMMIT, ...).
 *
 * This file exists only so issue #7 (libc shims) can prove its own "done
 * when" -- the full DOOM engine linking with zero undefined symbols --
 * without waiting on #8. Every function here is a no-op standing in for
 * real behavior; #8 replaces this file, it does not extend it. Same
 * pattern as #6's now-retired src/main_stub.c.
 */

#include <stdint.h>

#include "doomgeneric.h"

void DG_Init(void) {}
void DG_DrawFrame(void) {}
void DG_SleepMs(uint32_t ms) { (void)ms; }
uint32_t DG_GetTicksMs(void) { return 0; }
int DG_GetKey(int *pressed, unsigned char *key) {
  (void)pressed;
  (void)key;
  return 0; /* "no key event" -- matches KEYQ's empty-queue contract */
}
void DG_SetWindowTitle(const char *title) { (void)title; }
