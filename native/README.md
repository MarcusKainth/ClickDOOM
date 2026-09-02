# native

The native-mode crate: SQL text for DOOM's tic simulation and renderer, the
WAD directory reader that turns `doom1.wad` into raw lump rows, and the engine
constant tables. `NATIVE.md` at the repository root is the contract this crate
implements. Nothing here executes SQL; the driver does.

This crate is GPL-3.0-or-later: it copies the engine's tables and reproduces
its functions, and `native/LICENSE` carries the terms. `LICENSING.md` at the
root has the boundary with the rest of the tree.
