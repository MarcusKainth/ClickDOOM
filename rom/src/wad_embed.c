/* rom/src/wad_embed.c — connects the blob wad_embed.S embeds to
 * syscalls.c's virtual filesystem (issue #9). */

#include <stddef.h>

#include "rom_vfs.h"
#include "wad_embed.h"

extern const unsigned char _wad_doom1_start[];
extern const unsigned char _wad_doom1_end[];

void wad_embed_register(void) {
  rom_vfs_register("doom1.wad", _wad_doom1_start,
                   (size_t)(_wad_doom1_end - _wad_doom1_start));
}
