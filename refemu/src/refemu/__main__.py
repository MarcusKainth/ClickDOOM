"""`python -m refemu <image.bin> [--max-instructions N]` -- emit a SPEC §7
trace for a flat binary loaded at RAM_BASE. See `trace._main` for the
actual argument parsing and behavior; this file exists only so `-m refemu`
works without the "module already in sys.modules" double-import warning
that `-m refemu.trace` would trigger (refemu/__init__.py imports trace.py
eagerly to re-export its functions).
"""

from .trace import _main

if __name__ == "__main__":
    raise SystemExit(_main())
