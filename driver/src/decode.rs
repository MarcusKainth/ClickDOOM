//! Running `sqlcpu/decode.sql` against a specific database.
//!
//! `decode.sql` qualifies its own table references as `clickdoom.ram` and
//! `clickdoom.decoded`, so pointing it at a differently named database means
//! rewriting that qualifier. This does it once, as a plain text
//! substitution, in place of shelling out to `sed` for it. Its
//! `text_start_word`/`text_end_word` bounds are already real ClickHouse
//! query parameters, bound as such rather than interpolated into the text.

use crate::client::{Db, Error};
use crate::sql::split_statements;

const DECODE_SQL: &str = include_str!("../../sqlcpu/decode.sql");

/// Rebuilds `decoded` from `ram`'s current contents, over the text region
/// `[text_start_word, text_end_word)`.
pub async fn decode(
    db: &Db,
    database: &str,
    text_start_word: u32,
    text_end_word: u32,
) -> Result<(), Error> {
    let qualified = DECODE_SQL.replace("clickdoom.", &format!("{database}."));
    let params = [
        ("text_start_word", text_start_word),
        ("text_end_word", text_end_word),
    ];
    for statement in split_statements(&qualified) {
        db.run_with_params(statement, &params).await?;
    }
    Ok(())
}
