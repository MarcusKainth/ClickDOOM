/* rom/src/rom_vfs.h — the read-only virtual filesystem syscalls.c serves
 * file I/O from (issue #7 / SPEC §4).
 *
 * Issue #9 (embed the shareware WAD) calls rom_vfs_register() once per
 * file, before doomgeneric_Create() runs, to make that file visible to
 * open()/fopen() and everything built on top of them (w_file_stdc.c's
 * WAD reader in particular). `data`/`size` should point at a rodata blob
 * baked into the ROM image -- nothing here copies or takes ownership of
 * the bytes.
 *
 * The registry is intentionally tiny (see ROM_VFS_MAX_FILES in
 * syscalls.c): this ROM embeds one WAD, not a filesystem.
 */

#ifndef ROM_VFS_H
#define ROM_VFS_H

#include <stddef.h>

void rom_vfs_register(const char *name, const unsigned char *data, size_t size);

#endif /* ROM_VFS_H */
