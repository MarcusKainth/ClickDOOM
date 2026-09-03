//! Byte-for-byte comparison of `commit`'s generated SQL text against a
//! committed reference for the same inputs.

use clickdoom_executor::word::WordAddr;

use clickdoom_executor::commit::{
    console_out_flush_sql, cpu_state_flush_sql, fbpal_flush_sql, ram_flush_sql, retention_sql,
};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("golden/", $name, ".sql"))
    };
}

/// The batch every fixture below is generated for. Neither 0 nor 1, so a
/// generator that dropped the argument and hardcoded either would show up.
const BATCH: u64 = 7;

#[test]
fn ram_flush_default_matches_the_reference() {
    assert_eq!(
        ram_flush_sql("clickdoom", BATCH),
        fixture!("ram_flush_default")
    );
}

#[test]
fn ram_flush_other_db_matches_the_reference() {
    assert_eq!(
        ram_flush_sql("other_db", BATCH),
        fixture!("ram_flush_other_db")
    );
}

#[test]
fn fbpal_flush_default_matches_the_reference() {
    assert_eq!(
        fbpal_flush_sql("clickdoom", BATCH),
        fixture!("fbpal_flush_default")
    );
}

#[test]
fn console_out_flush_default_matches_the_reference() {
    assert_eq!(
        console_out_flush_sql("clickdoom", BATCH),
        fixture!("console_out_flush_default")
    );
}

#[test]
fn cpu_state_flush_default_matches_the_reference() {
    assert_eq!(
        cpu_state_flush_sql("clickdoom", BATCH),
        fixture!("cpu_state_flush_default")
    );
}

#[test]
fn retention_default_matches_the_reference() {
    assert_eq!(
        retention_sql("clickdoom", BATCH, 16),
        fixture!("retention_default")
    );
}

#[test]
fn retention_n10_matches_the_reference() {
    assert_eq!(
        retention_sql("clickdoom", BATCH, 10),
        fixture!("retention_n10")
    );
}

/// Every statement carries the batch it was given, and none derives one of
/// its own. A statement that read `max(batch_id)` back would be
/// byte-identical across batches, which is the shape the explicit argument
/// exists to rule out.
#[test]
fn every_statement_names_the_batch_it_was_given() {
    for (name, at_seven, at_eight) in [
        (
            "ram_flush",
            ram_flush_sql("clickdoom", 7),
            ram_flush_sql("clickdoom", 8),
        ),
        (
            "fbpal_flush",
            fbpal_flush_sql("clickdoom", 7),
            fbpal_flush_sql("clickdoom", 8),
        ),
        (
            "console_out_flush",
            console_out_flush_sql("clickdoom", 7),
            console_out_flush_sql("clickdoom", 8),
        ),
        (
            "cpu_state_flush",
            cpu_state_flush_sql("clickdoom", 7),
            cpu_state_flush_sql("clickdoom", 8),
        ),
        (
            "retention",
            retention_sql("clickdoom", 7, 16),
            retention_sql("clickdoom", 8, 16),
        ),
    ] {
        assert_ne!(at_seven, at_eight, "{name} ignores its batch_id");
        assert!(
            !at_seven.contains("max(batch_id)"),
            "{name} still derives a batch id of its own"
        );
    }
}

/// The write-log carries `wl_addr` relative to the image's base and `ram`
/// is keyed absolutely, so the flush has to put the base back on. Without
/// it every store lands about 536 million words below the image, where it
/// sorts ahead of every real row instead of replacing one: no error, and a
/// later load reads the value the ROM was built with.
#[test]
fn the_ram_flush_rebases_the_write_log_onto_rams_own_key() {
    let base = WordAddr::ram_base().get();
    let sql = ram_flush_sql("clickdoom", BATCH);
    assert_eq!(base, 536_870_912);
    assert!(
        sql.contains(&format!("SELECT {base} + t.1,")),
        "the flush writes a RAM_BASE-relative index into ram.word_addr:\n{sql}"
    );
    // The framebuffer and palette logs are already in their own regions'
    // domains, so the same addition there would move every pixel.
    let fbpal = fbpal_flush_sql("clickdoom", BATCH);
    assert!(
        !fbpal.contains(&base.to_string()),
        "the fb/palette flush rebased an address that was already region-relative:\n{fbpal}"
    );
}
