/* rom/src/syscalls.c — libc shims for doomgeneric (issue #7).
 *
 * There is no OS here (SPEC §1: machine-mode only, no MMU, no interrupts).
 * Rather than reimplement malloc/printf/string.h by hand, we link the
 * toolchain's bundled newlib and provide only the small, well-known
 * syscall surface it calls down into -- the same shape every bare-metal
 * newlib port uses. This file is the entire boundary between "the DOOM
 * engine's idea of a libc" and SPEC's actual machine: the heap (§2), the
 * debug console (§3 PUTCHAR), file reads served from an embedded blob
 * (the WAD, wired up by #9 via rom_vfs_register()), and a clean stop (§3
 * EXIT).
 *
 * Every function here is a plain C function operating on RAM and MMIO
 * addresses through ordinary load/store -- SPEC §1's fatal-halt list
 * (ecall/ebreak/CSR) is never touched. That's not an incidental property;
 * it's the actual point of writing shims instead of trusting an
 * arbitrary libc's own low-level internals.
 */

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include "rom_vfs.h"

/* Pull in the real prototypes for the syscalls below so a signature
 * mismatch against what newlib actually expects is a compile error here,
 * not a mystery at link or run time. */

/* -- SPEC §3 MMIO registers -- */
#define MMIO_BASE 0x10000000u
#define REG_TICKS_MS (*(volatile uint32_t *)(MMIO_BASE + 0x00))
#define REG_KEYQ (*(volatile uint32_t *)(MMIO_BASE + 0x04))
#define REG_EXIT (*(volatile uint32_t *)(MMIO_BASE + 0x08))
#define REG_PUTCHAR (*(volatile uint32_t *)(MMIO_BASE + 0x0C))
#define REG_FRAME_COMMIT (*(volatile uint32_t *)(MMIO_BASE + 0x10))

/* -- heap (SPEC §2), bounds from toolchain/link.ld -- */
extern char __heap_start[];
extern char __heap_end[];
static char *brk_ptr = 0;

void *_sbrk(ptrdiff_t incr) {
  if (brk_ptr == 0) {
    brk_ptr = __heap_start;
  }
  char *old = brk_ptr;
  char *next = old + incr;
  /* Compare as addresses, not as a signed length -- incr can be
   * negative (rare, but valid POSIX sbrk usage) and this still has to
   * reject both directions correctly. */
  if (next < __heap_start || next > __heap_end) {
    errno = ENOMEM;
    return (void *)-1;
  }
  brk_ptr = next;
  return old;
}

/* -- read-only virtual filesystem, backing "file I/O served from the
 * embedded WAD" (SPEC §4/§9). Empty until #9 calls rom_vfs_register(); an
 * empty table is a completely valid state -- every open() just reports
 * ENOENT, which is what "no files exist yet" should do. -- */

#define ROM_VFS_MAX_FILES 8
#define ROM_VFS_MAX_FDS 16

struct rom_vfs_entry {
  const char *name;
  const unsigned char *data;
  size_t size;
};

static struct rom_vfs_entry vfs_files[ROM_VFS_MAX_FILES];
static int vfs_file_count = 0;

struct rom_fd {
  int in_use;
  int is_sink; /* opened for write: discard everything, read as EOF */
  const unsigned char *data;
  size_t size;
  size_t pos;
};

static struct rom_fd fds[ROM_VFS_MAX_FDS];

void rom_vfs_register(const char *name, const unsigned char *data,
                      size_t size) {
  if (vfs_file_count >= ROM_VFS_MAX_FILES) {
    return; /* nowhere to put it; caller's problem to notice */
  }
  vfs_files[vfs_file_count].name = name;
  vfs_files[vfs_file_count].data = data;
  vfs_files[vfs_file_count].size = size;
  vfs_file_count++;
}

static const struct rom_vfs_entry *vfs_lookup(const char *path) {
  /* DOOM opens files by whatever path W_AddFile/fopen was given, which
   * is usually just a bare filename (e.g. "doom1.wad") in this
   * environment -- match on the path tail so a caller-supplied "./"
   * or directory prefix doesn't cause a spurious miss. */
  size_t path_len = strlen(path);
  for (int i = 0; i < vfs_file_count; i++) {
    size_t name_len = strlen(vfs_files[i].name);
    if (name_len > path_len) {
      continue;
    }
    if (strcmp(path + (path_len - name_len), vfs_files[i].name) == 0) {
      return &vfs_files[i];
    }
  }
  return 0;
}

static int alloc_fd(void) {
  /* fds 0-2 are reserved for stdin/stdout/stderr and never live in this
   * table -- start allocating from 3, matching real fd numbering. */
  for (int i = 0; i < ROM_VFS_MAX_FDS; i++) {
    if (!fds[i].in_use) {
      fds[i].in_use = 1;
      return i + 3;
    }
  }
  return -1;
}

static struct rom_fd *fd_lookup(int fd) {
  int i = fd - 3;
  if (i < 0 || i >= ROM_VFS_MAX_FDS || !fds[i].in_use) {
    return 0;
  }
  return &fds[i];
}

int _open(const char *path, int flags, ...) {
  if (flags & (O_WRONLY | O_RDWR | O_CREAT)) {
    /* No writable storage exists (SPEC §2 has no disk region). Rather
     * than fail every config/save-game write and risk DOOM treating
     * that as fatal at startup, hand back a sink fd: writes succeed
     * and vanish, reads see EOF. Genuinely nothing is persisted --
     * there is nowhere for it to go -- but "the write call failed"
     * and "the write call succeeded into the void" are different
     * failure modes for a game that only checks the former. */
    int fd = alloc_fd();
    if (fd < 0) {
      errno = EMFILE;
      return -1;
    }
    struct rom_fd *f = fd_lookup(fd);
    f->is_sink = 1;
    f->data = 0;
    f->size = 0;
    f->pos = 0;
    return fd;
  }

  const struct rom_vfs_entry *entry = vfs_lookup(path);
  if (!entry) {
    errno = ENOENT;
    return -1;
  }
  int fd = alloc_fd();
  if (fd < 0) {
    errno = EMFILE;
    return -1;
  }
  struct rom_fd *f = fd_lookup(fd);
  f->is_sink = 0;
  f->data = entry->data;
  f->size = entry->size;
  f->pos = 0;
  return fd;
}

int _close(int fd) {
  if (fd < 3) {
    return 0; /* closing a std stream is a harmless no-op here */
  }
  struct rom_fd *f = fd_lookup(fd);
  if (!f) {
    errno = EBADF;
    return -1;
  }
  f->in_use = 0;
  return 0;
}

ssize_t _read(int fd, void *buf, size_t len) {
  if (fd == 0) {
    return 0; /* no stdin device; every read is EOF */
  }
  if (fd == 1 || fd == 2) {
    errno = EBADF;
    return -1;
  }
  struct rom_fd *f = fd_lookup(fd);
  if (!f) {
    errno = EBADF;
    return -1;
  }
  if (f->is_sink || f->pos >= f->size) {
    return 0;
  }
  size_t remaining = f->size - f->pos;
  size_t n = (len < remaining) ? len : remaining;
  memcpy(buf, f->data + f->pos, n);
  f->pos += n;
  return (ssize_t)n;
}

ssize_t _write(int fd, const void *buf, size_t len) {
  if (fd == 1 || fd == 2) {
    /* SPEC §3: PUTCHAR appends the low byte of each write to
     * console_out. One MMIO store per byte -- there is no batched
     * "write a buffer" register, by design (SPEC §3's table). */
    const unsigned char *p = buf;
    for (size_t i = 0; i < len; i++) {
      REG_PUTCHAR = p[i];
    }
    return (ssize_t)len;
  }
  struct rom_fd *f = fd_lookup(fd);
  if (!f) {
    errno = EBADF;
    return -1;
  }
  if (f->is_sink) {
    return (ssize_t)len; /* discarded, per _open's sink-fd contract */
  }
  /* Non-sink fds come only from vfs_lookup(), which is always a
   * read-only WAD entry -- writing to one is a bug in the caller, not
   * something to silently allow. */
  errno = EBADF;
  return -1;
}

off_t _lseek(int fd, off_t offset, int whence) {
  struct rom_fd *f = fd_lookup(fd);
  if (!f) {
    errno = EBADF;
    return -1;
  }
  off_t base;
  switch (whence) {
  case SEEK_SET:
    base = 0;
    break;
  case SEEK_CUR:
    base = (off_t)f->pos;
    break;
  case SEEK_END:
    base = (off_t)f->size;
    break;
  default:
    errno = EINVAL;
    return -1;
  }
  off_t next = base + offset;
  if (next < 0 || (size_t)next > f->size) {
    errno = EINVAL;
    return -1;
  }
  f->pos = (size_t)next;
  return next;
}

int _fstat(int fd, struct stat *st) {
  memset(st, 0, sizeof(*st));
  if (fd < 3) {
    st->st_mode = S_IFCHR; /* std streams: character-device-shaped */
    return 0;
  }
  struct rom_fd *f = fd_lookup(fd);
  if (!f) {
    errno = EBADF;
    return -1;
  }
  st->st_mode = S_IFREG;
  st->st_size = (off_t)f->size;
  return 0;
}

int _isatty(int fd) {
  (void)fd;
  return 0; /* nothing here is a terminal */
}

int _link(const char *oldpath, const char *newpath) {
  (void)oldpath;
  (void)newpath;
  errno = EROFS; /* the WAD-backed VFS has no writable namespace */
  return -1;
}

int _unlink(const char *path) {
  (void)path;
  errno = EROFS;
  return -1;
}

int mkdir(const char *path, mode_t mode) {
  (void)path;
  (void)mode;
  /* No real filesystem to create a directory in, and nothing downstream
   * persists into one (see _open's sink-fd handling) -- report success
   * so callers that only check for a hard failure don't treat "there is
   * no disk" as fatal. */
  return 0;
}

/* -- process control -- */

int _getpid(void) { return 1; /* one "process", always */ }

void _exit(int status) {
  /* SPEC §3: EXIT halts emulation; written value = exit code. This is
   * the only correct way to stop -- there is no OS to return to, and
   * SPEC §1 makes ecall/ebreak fatal halts, not syscalls. */
  REG_EXIT = (uint32_t)status;
  for (;;) {
    /* REG_EXIT already halted the emulator; spin in case anything
     * ever calls this from a context where that hasn't landed yet. */
  }
}

int _kill(int pid, int sig) {
  (void)pid;
  /* newlib's abort()/raise() route through kill(getpid(), sig) before
   * falling back to _exit(). Since there is exactly one "process" and
   * no signal delivery mechanism, treat any signal to ourselves as an
   * immediate abnormal stop rather than pretending to deliver it. */
  _exit(128 + sig);
}
