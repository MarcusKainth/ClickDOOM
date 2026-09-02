//! Regenerates the engine's constant tables from the vendored C source.
//!
//!     cargo run -p clickdoom-native --bin gen_tables -- \
//!         rom/vendor/doomgeneric/doomgeneric native/tables
//!
//! `native/tables/README.md` names the source each table comes from. The
//! regeneration test in `native/tests/tables.rs` runs this same code
//! against a temporary directory and fails if the result differs from
//! what is committed.

use std::path::PathBuf;
use std::process::ExitCode;

use clickdoom_native::tables::generate;

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [source, out] = args.as_slice() else {
        eprintln!("usage: gen_tables <doomgeneric source dir> <output dir>");
        return ExitCode::FAILURE;
    };
    match generate::write_all(source, out) {
        Ok(written) => {
            for name in written {
                println!("{}", out.join(name).display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gen_tables: {error}");
            ExitCode::FAILURE
        }
    }
}
