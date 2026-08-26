/* rom/src/wad_embed.h — issue #9. */
#ifndef WAD_EMBED_H
#define WAD_EMBED_H

/* Registers the embedded doom1.wad with syscalls.c's virtual filesystem
 * (rom_vfs_register). Must be called before doomgeneric_Create() -- the
 * engine's IWAD search happens synchronously inside it. Deliberately an
 * explicit call from main(), not a __attribute__((constructor)): crt0
 * never processes .init_array (see toolchain/link.ld's /DISCARD/, added
 * in #7), so a constructor here would compile, link, and never run. */
void wad_embed_register(void);

#endif /* WAD_EMBED_H */
