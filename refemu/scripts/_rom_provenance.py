"""Shared ROM-provenance discipline for refemu's trace-generation scripts
(`gen_reference_trace.py`, `gen_demo3_trace.py`): the `PINNED_HASH`
assertion and the hash-in-filename convention, in one place so the two
scripts can't silently drift on either.

This exists because of issue #96's own incident: `gen_reference_trace.py`
generated a reference against the attract-mode ROM hours before #111
(`-timedemo demo3` argv) superseded it, and the failure mode that made
that dangerous -- a same-named output file silently looking current after
the ROM changed underneath it -- is exactly what both functions below
prevent. `gen_demo3_trace.py` (issue #129's harness, prepared but not run
per the team lead's explicit "do not start the run") needs the identical
discipline, not a close reimplementation of it, so it imports these
rather than re-deriving them.
"""

from __future__ import annotations

import hashlib
from pathlib import Path


class UnpinnedRomError(RuntimeError):
    """Raised when a ROM image's sha256 doesn't match the pinned hash it
    was checked against. Always fatal for callers -- never generate a
    reference (or resume a partial one) against an unpinned ROM."""


def assert_pinned_hash(image: bytes, pinned_hash_path: Path) -> str:
    """Returns the ROM's sha256 hex digest if it matches
    `pinned_hash_path`'s content; raises `UnpinnedRomError` (with both
    values, for a useful message) otherwise."""
    pinned = pinned_hash_path.read_text().strip()
    actual = hashlib.sha256(image).hexdigest()
    if actual != pinned:
        raise UnpinnedRomError(
            f"ROM does not match {pinned_hash_path}\n  pinned: {pinned}\n  actual: {actual}"
        )
    return actual


def hashed_filename(prefix: str, rom_sha256: str, suffix: str) -> str:
    """`<prefix>.<rom sha256[:12]><suffix>` -- the ROM's own hash prefix
    embedded directly in the filename (12 hex chars, matching `git`'s
    short-hash length), not only recorded in a sidecar. Two ROMs can
    never share one output file under this convention, so a stale trace
    can't be mistaken for a fresh one by name alone."""
    return f"{prefix}.{rom_sha256[:12]}{suffix}"
