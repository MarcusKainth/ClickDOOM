//! Byte-for-byte comparison of `commit`'s generated SQL text against the
//! Python original's output for the same inputs.

use clickdoom_executor::commit::{
    console_out_flush_sql, cpu_state_flush_sql, fbpal_flush_sql, ram_flush_sql, retention_sql,
};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("golden/", $name, ".sql"))
    };
}

#[test]
fn ram_flush_default_matches_python_output() {
    assert_eq!(ram_flush_sql("clickdoom"), fixture!("ram_flush_default"));
}

#[test]
fn ram_flush_other_db_matches_python_output() {
    assert_eq!(ram_flush_sql("other_db"), fixture!("ram_flush_other_db"));
}

#[test]
fn fbpal_flush_default_matches_python_output() {
    assert_eq!(
        fbpal_flush_sql("clickdoom"),
        fixture!("fbpal_flush_default")
    );
}

#[test]
fn console_out_flush_default_matches_python_output() {
    assert_eq!(
        console_out_flush_sql("clickdoom"),
        fixture!("console_out_flush_default")
    );
}

#[test]
fn cpu_state_flush_default_matches_python_output() {
    assert_eq!(
        cpu_state_flush_sql("clickdoom"),
        fixture!("cpu_state_flush_default")
    );
}

#[test]
fn retention_default_matches_python_output() {
    assert_eq!(
        retention_sql("clickdoom", 16),
        fixture!("retention_default")
    );
}

#[test]
fn retention_n10_matches_python_output() {
    assert_eq!(retention_sql("clickdoom", 10), fixture!("retention_n10"));
}
