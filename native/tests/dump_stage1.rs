//! Writes the first statement out in the form a shell can reissue, for
//! measuring what the same tic costs against tables of different sizes.
//!
//! Not a check. It asserts nothing and exists so that
//! `scripts/table-size.sh` issues the statement the project generates
//! rather than one written by hand beside it.
//!
//! Behind the `clickhouse-tests` feature with the rest of the measuring
//! machinery, though it needs no server of its own.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::tick::{Input, bench};

/// Where the statement lands. `scripts/table-size.sh` reads this path.
const OUT: &str = "target/stage1_tic21.sql";

#[test]
fn the_first_statement_for_tic_21_is_written_out() {
    let sql = bench::stage1("clickdoom", None, &[Input::demo(21)]).sql;
    std::fs::write(OUT, &sql).unwrap_or_else(|e| panic!("writing {OUT}: {e}"));
    println!("{OUT}: {} bytes", sql.len());
}
