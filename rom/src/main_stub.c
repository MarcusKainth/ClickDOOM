/* Placeholder main for the crt0/linker-script smoke test (issue #6).
 *
 * This is NOT doomgeneric's main — that lands with the DG_* platform hooks
 * (issue #8) once libc shims (issue #7) exist. It exists only to give
 * crt0's `call main` a real C symbol to resolve and to keep touching a
 * variable that lives in .bss, so that section isn't linked away and the
 * crt0 -> main handoff is exercised by every build until doomgeneric
 * replaces this file.
 */

static volatile unsigned int bss_counter;

int main(void) {
  bss_counter++;

  for (;;) {
    /* no OS to return to */
  }

  return 0;
}
