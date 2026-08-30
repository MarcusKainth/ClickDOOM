//! Byte-for-byte comparison of `checkpoint`'s generated SQL against
//! Python's `scripts/checkpoint_query.py`'s own output for the same input.

use clickdoom_driver::checkpoint::{checkpoint_sql, reg_checkpoint_sql};

#[test]
fn checkpoint_sql_matches_python_output() {
    assert_eq!(
        checkpoint_sql("testdb"),
        include_str!("fixtures/checkpoint/checkpoint_full.sql")
    );
}

#[test]
fn reg_checkpoint_sql_matches_python_output() {
    assert_eq!(
        reg_checkpoint_sql("testdb"),
        include_str!("fixtures/checkpoint/checkpoint_reg.sql")
    );
}
