# Security policy

## Reporting a vulnerability

**Report privately, through
[GitHub's private vulnerability reporting](https://github.com/MarcusKainth/ClickDOOM/security/advisories/new).**
That opens a draft advisory visible only to the maintainers.

Please do not open a public issue or a pull request for something you believe is
exploitable.

### What to include

- The commit, and the ClickHouse version you ran against.
- What an attacker gains, and what they need in order to reach it.
- The smallest reproduction you have. A ROM that triggers it, or the sequence of
  statements, is enough. A working exploit is not required.

A report that turns out to be a non-issue costs one reply.

### What to expect

Response targets for a single-maintainer project:

| | |
|---|---|
| Acknowledgement | within 3 working days |
| Initial assessment | within 10 working days |
| Fix or documented mitigation | depends on severity, discussed with you in the advisory |

You will be credited in the advisory unless you ask not to be. There is no bug
bounty.

## What counts as a vulnerability here

ClickDOOM is an RV32IM emulator. It executes an arbitrary binary inside
ClickHouse and drives it from a local process. The boundary that matters is
between the emulated machine and the host running it.

**In scope**

- A way for the emulated program to reach outside the emulator: run code on the
  host, read or write files, open a network connection, or touch ClickHouse
  state beyond the tables SPEC.md gives it.
- A way for a crafted ROM to make the SQL CPU execute something other than the
  instruction it decoded, in a manner that escapes the emulated address space.
- Code execution through the build path. `make build-rom` runs a pinned Docker
  container over `rom/vendor/`, `rom/patches/` and `rom/src/`.
- A workflow change that lets a pull request from a fork obtain write access,
  read a secret, or alter what CI proves.
- Credentials leaking into logs, CI output, or committed benchmark results.

**Not in scope**

- The `clickdoom` password in `docker-compose.yml` and in the workflows. It
  guards an emulator's RAM on a local container, holds nothing secret, and is
  documented as local-only where it appears.
- DOOM's own 1993 defects, faithfully reproduced. A buffer overflow inside the
  emulated program is the emulator working correctly. It is only a
  vulnerability if it escapes the emulated address space.
- Vulnerabilities in ClickHouse itself. Report those to ClickHouse.
- Anything requiring an attacker who can already run SQL against your server as
  `default`, or run commands on your machine.
- Resource exhaustion from parameters you chose, such as a batch size larger
  than available memory.

## Supported versions

There are no releases. The tip of `main` is the supported version, and a fix
lands there.

## How the project defends itself

Each control is recorded where it is enforced:

- The ROM build is reproducible and pinned. `rom/PINNED_HASH` records the
  sha256 of the built binary, `make -C rom check-pinned-hash` verifies it, and
  CI verifies it independently of the build job. A mismatch is treated as a P0
  nondeterministic-build incident under SPEC.md.
- Vendored upstream sources carry a sha256 manifest taken at import time,
  `rom/vendor/doomgeneric.sha256sums`, checkable without git history. Upstream
  is pinned to a commit SHA and fetched by `git clone`, never from a generated
  archive URL.
- The embedded WAD is pinned by sha256 in `rom/wad/doom1.wad.sha256sum`.
- ClickHouse is pinned by image digest in `docker-compose.yml` and by version in
  the workflows. Bumping the pin needs a `ci:` pull request with nightly
  deep-diff evidence.
- `scripts/check_purity.sh` runs on every pull request and fails the build on
  any mechanism that delegates computation to a subprocess, which is the same
  boundary a sandbox escape would have to cross.
- Secret scanning and push protection are enabled on the repository.
