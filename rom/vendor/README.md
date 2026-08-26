# rom/vendor/ — pristine upstream sources

Issue #41. This directory holds unmodified upstream source, imported once
and never edited after that. Every deviation the ClickDOOM port requires
lives in `rom/patches/` instead and is applied at build time — never here.
That split is what makes PURITY.md's claim ("the game engine sources are
upstream, patched only where the port requires") checkable by inspection
rather than something a reviewer has to take on trust: diff `doomgeneric/`
below against the pinned upstream commit and it matches, file for file.

## doomgeneric

- **Upstream:** <https://github.com/ozkl/doomgeneric>
- **Pinned commit:** `dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284` (`master`,
  fetched 2026-08-26)
- **License:** GPL-2.0-or-later. Full text at
  [`doomgeneric/LICENSE`](doomgeneric/LICENSE); also see
  [`doomgeneric/README.TXT`](doomgeneric/README.TXT), the original id
  Software DOOM source release notes carried over from upstream. This
  covers the DOOM engine and the doomgeneric platform layer. The shareware
  `doom1.wad` this ROM embeds is a **separate** matter under its own
  redistribution terms (issue #9) — not covered by, or related to, this
  GPL grant.

doomgeneric already contains the full DOOM engine source (id Software's
GPL release, restructured with the platform-independent `DG_*` hooks
CLAUDE.md and SPEC §3 build against) inside its own `doomgeneric/`
subdirectory — there is no separate upstream engine repo to vendor. Hence
the doubled path, `vendor/doomgeneric/doomgeneric/`: the outer directory is
this vendor entry, the inner one is upstream's own layout, kept exactly as
upstream has it.

### Fetched via `git clone` + `git checkout <sha>`, not a GitHub archive URL

Deliberate, not an oversight (see issue #41's discussion). GitHub's
`/archive/<ref>.tar.gz` endpoint generates the tarball on demand and its
compression has changed across GitHub infrastructure updates in the past —
a sha256 pinned against one today is not guaranteed to match the same
endpoint later, with no upstream content change at all. That failure would
land as `rom/PINNED_HASH` going red on an unrelated PR, which SPEC §4/§8
say to treat as a P0 nondeterministic-build incident — a phantom worth
avoiding entirely by never depending on that endpoint. A real `git clone`
resolves actual git objects addressed by content hash, which is what's
pinned above.

### What's excluded from the mirror, and why

Everything under upstream's repo root is vendored **except**:

- `screenshots/` — binary PNG marketing images, not source, no bearing on
  the build or the "unmodified except for X" claim.
- `doomgeneric.sln` — root-level Visual Studio solution file, Windows-only
  tooling irrelevant to a bare-metal RV32IM target. (The `.vcxproj`/
  `.vcxproj.filters` files *inside* `doomgeneric/doomgeneric/` are kept —
  that subtree is vendored wholesale, unmodified, no exceptions, so it
  diffs clean against upstream.)
- `doomgeneric/.gitignore` (the outer, repo-root one) — found the hard way:
  it contains a bare `doomgeneric` pattern (meant upstream to ignore their
  own build output binary), and a `.gitignore` inside a vendored directory
  is not inert — git honors it for that subtree. Kept as-is, it silently
  excluded the entire 202-file `doomgeneric/doomgeneric/` engine source
  from `git add`, with no error, no warning. First import commit here had
  6 files in it instead of ~205 for exactly this reason, caught only by
  checking the file count rather than trusting a clean `git commit` output.
  Dropped rather than kept-and-worked-around, since it has zero purpose in
  this tree (we don't build via upstream's Makefiles or produce their
  `doomgeneric` binary) and any working-around would be more fragile than
  just not vendoring a file whose entire content is "ignore paths that
  don't apply here."

Nothing under `doomgeneric/doomgeneric/` (the actual engine + platform
source, 205 files) is excluded or altered. If a later issue needs
`screenshots/` or the `.sln` for some reason, add them in a follow-up
"vendor: add ..." commit against the same pinned commit — never retroactively
edit this import.

### Integrity

`doomgeneric.sha256sums` records a sha256 for every vendored file, taken at
import time, independent of git's own content-addressing — so the pristine
claim can be checked without a git history at all (e.g. after a shallow
clone). Verify with:

    cd rom/vendor && shasum -a 256 -c doomgeneric.sha256sums

## Adding another vendored project

Same pattern: `git clone` the upstream repo (never a generated archive
URL), `git checkout` a specific commit SHA, copy the tree in as its own
commit, record the commit SHA and license here, and generate a sha256
manifest the same way.
