//! Byte-for-byte comparison of `checkpoint`'s generated SQL against
//! Python's `scripts/checkpoint_query.py`'s own output for the same input.

use clickdoom_driver::checkpoint::{batch_checkpoints_sql, checkpoint_sql, reg_checkpoint_sql};

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

#[test]
fn batch_checkpoints_sql_matches_the_reference() {
    assert_eq!(
        batch_checkpoints_sql("testdb", 42),
        include_str!("fixtures/checkpoint/batch_checkpoints.sql")
    );
}
