"""Read a refemu snapshot.

One reader for both kinds the emulator writes: a whole machine, and a single
frame's pixels. Dependency-free and importable under a plain `python3`, so a
seeding script does not need the emulator's own environment to read one
integer out of a header.

    from refemu_snapshot import load
    header, sections = load(path, need=("ram", "framebuffer", "palette"))

Every section is checked against the sha256 its header states. A name in
`need` that the file does not carry is an error here rather than an empty
region several steps later.
"""

import hashlib
import json

FORMAT_VERSION = 3
MAGIC = b"REFEMU-SNAPSHOT %d\n" % FORMAT_VERSION


class SnapshotError(Exception):
    """The file is not a snapshot this reader accepts."""


def load(path, need=()):
    """Return (header, {section name: bytes}) for the snapshot at `path`."""
    with open(path, "rb") as handle:
        magic = handle.read(len(MAGIC))
        if magic != MAGIC:
            claimed = _claimed_version(magic)
            if claimed is not None and claimed != FORMAT_VERSION:
                raise SnapshotError(
                    f"{path}: format version {claimed}, and this reads {FORMAT_VERSION}"
                )
            raise SnapshotError(f"{path}: not a refemu snapshot v{FORMAT_VERSION}")
        header = json.loads(handle.readline())
        sections = {}
        for section in header["sections"]:
            handle.seek(section["offset"])
            data = handle.read(section["length"])
            if len(data) != section["length"]:
                raise SnapshotError(
                    f"{path}: section {section['name']} is {len(data)} bytes, "
                    f"and its header says {section['length']}"
                )
            if hashlib.sha256(data).hexdigest() != section["sha256"]:
                raise SnapshotError(
                    f"{path}: section {section['name']} does not match its own sha256"
                )
            sections[section["name"]] = data

    missing = [name for name in need if name not in sections]
    if missing:
        raise SnapshotError(f"{path} carries no section named {', '.join(missing)}")
    return header, sections


def _claimed_version(magic):
    """The version a file says it is, for one this reader will not accept."""
    text = magic.decode("utf-8", "replace")
    if not text.startswith("REFEMU-SNAPSHOT "):
        return None
    try:
        return int(text[len("REFEMU-SNAPSHOT "):].strip())
    except ValueError:
        return None
