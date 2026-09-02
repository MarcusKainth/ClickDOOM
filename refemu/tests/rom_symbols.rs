//! The pinned ROM's symbol table, read by the ELF reader.
//!
//! The synthetic tests in `image.rs` cover the parser against bytes. This one
//! covers it against the file it exists for, so a reader that keeps the wrong
//! symbols fails here rather than in whatever reads them.
//!
//! Behind the `rom-tests` feature, so a run either includes it or visibly does
//! not.
#![cfg(feature = "rom-tests")]

use std::path::{Path, PathBuf};

use refemu::{Image, SymbolKind};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn pinned_elf() -> Image {
    let path = repo().join("rom").join("build").join("doom-rv32im.elf");
    assert!(
        path.exists(),
        "this needs {}, which is not built. Run `make build-rom`.",
        path.display()
    );
    Image::parse_elf(&std::fs::read(&path).unwrap()).unwrap()
}

#[test]
fn the_engine_globals_are_found_by_name_and_kind() {
    let image = pinned_elf();
    for (name, kind) in [
        // Code, which the reader already kept.
        ("P_Random", SymbolKind::Function),
        ("P_MobjThinker", SymbolKind::Function),
        // Global data.
        ("gametic", SymbolKind::Object),
        ("players", SymbolKind::Object),
        ("thinkercap", SymbolKind::Object),
        ("states", SymbolKind::Object),
        // File-local data, which has no binding a linker would export.
        ("oldweaponsowned", SymbolKind::Object),
        ("message_nottobefuckedwith", SymbolKind::Object),
    ] {
        let symbol = image
            .symbol(name)
            .unwrap_or_else(|| panic!("{name} is not in the symbol table"));
        assert_eq!(symbol.kind, kind, "{name}");
        assert!(symbol.size > 0, "{name} has no size");
    }
}

#[test]
fn an_address_inside_a_function_names_that_function() {
    let image = pinned_elf();
    let random = image.symbol("P_Random").unwrap().clone();
    for offset in [0, 4, random.size - 4] {
        assert_eq!(
            image
                .function_containing(random.addr + offset)
                .map(|s| s.name.as_str()),
            Some("P_Random"),
            "at P_Random+{offset}"
        );
    }
    // A data address is in no function.
    let gametic = image.symbol("gametic").unwrap();
    assert_eq!(image.function_containing(gametic.addr), None);
}
