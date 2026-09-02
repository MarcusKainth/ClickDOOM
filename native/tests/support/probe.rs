//! Loading the reference emulator's probe rows into `native_state`.
//!
//! A probe row carries the same fields as a state row, in the same order,
//! keyed by the frame it was taken at rather than by the tic. The staging
//! table it lands in takes every column as `Int64`, `Array(Int64)` or
//! `String`, because the probe writes -1 in places the schema declares
//! unsigned; the insert that follows casts each column back to what
//! `native_state` declares.
//!
//! The column list comes from `system.columns`, so it cannot drift from
//! `native/schema.sql`.

use clickdoom_native::sql::Statement;
use clickhouse::Row;
use serde::Deserialize;

use super::db::Fixture;

/// The columns of `native_state` a probe row carries, in order.
const NOT_PROBED: [&str; 5] = ["tic", "unresolved", "unimplemented", "dbg_ran", "dbg_prnd"];

#[derive(Row, Deserialize)]
struct Column {
    name: String,
    r#type: String,
}

/// Loads probe rows into `native_state`, keyed by the gametic each was taken
/// at. `tsv` is the rows themselves, one per line, without the comment
/// header.
pub async fn load(fixture: &Fixture, tsv: &[u8]) {
    let db = &fixture.database;
    let columns: Vec<Column> = fixture
        .rows(&format!(
            "SELECT name, type FROM system.columns \
             WHERE database = '{db}' AND table = 'native_state' AND name NOT IN {} \
             ORDER BY position",
            list(&NOT_PROBED)
        ))
        .await;

    let staging = columns
        .iter()
        .map(|c| format!("{} {}", c.name, permissive(&c.r#type)))
        .collect::<Vec<_>>()
        .join(", ");
    let cast = columns
        .iter()
        .map(|c| format!("CAST({}, '{}')", c.name, c.r#type))
        .collect::<Vec<_>>()
        .join(", ");

    let plan = [
        Statement::sql(format!(
            "CREATE TABLE {db}.probe_state \
             (frame_index UInt32, gametic UInt32, fb_hash String, {staging}) \
             ENGINE = MergeTree ORDER BY gametic"
        )),
        Statement::data(
            format!("INSERT INTO {db}.probe_state FORMAT TSV"),
            tsv.to_vec(),
        ),
        Statement::sql(format!(
            "INSERT INTO {db}.native_state \
             SELECT gametic AS tic, {cast}, 0, 0, [], [] FROM {db}.probe_state"
        )),
    ];
    fixture.execute(&plan).await.expect("the probe rows load");
}

/// The staging type for a column: wide enough that a value the probe writes
/// outside the schema's own range still reads.
fn permissive(declared: &str) -> &'static str {
    match declared {
        "String" => "String",
        t if t.starts_with("Array") => "Array(Int64)",
        _ => "Int64",
    }
}

fn list(names: &[&str]) -> String {
    format!(
        "({})",
        names
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
