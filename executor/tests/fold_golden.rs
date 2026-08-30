//! Byte-for-byte comparison of `fold`'s generated SQL text against the
//! Python original's output for the same inputs.

use clickdoom_executor::fold::{
    BatchArgs, SelectOnlyArgs, batch, halt_reason_transform, select_only,
};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("golden/", $name, ".sql"))
    };
}

/// Panics with the byte offset and surrounding context of the first
/// mismatch, rather than dumping two multi-kilobyte strings.
fn assert_matches_fixture(actual: &str, expected: &str, case: &str) {
    if actual == expected {
        return;
    }
    let mismatch = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| actual.len().min(expected.len()));
    let ctx = 60;
    let a_lo = mismatch.saturating_sub(ctx);
    let a_hi = (mismatch + ctx).min(actual.len());
    let e_hi = (mismatch + ctx).min(expected.len());
    panic!(
        "{case}: mismatch at byte {mismatch} (actual len {}, expected len {})\n\
         actual:   ...{:?}...\n\
         expected: ...{:?}...",
        actual.len(),
        expected.len(),
        &actual[a_lo..a_hi],
        &expected[a_lo..e_hi],
    );
}

const TEXT_WORDS_DEFAULT: u32 = 524_288;
const RAM_WORDS_DEFAULT: u32 = 6_291_456;
const HWM_DEFAULT: u32 = 20_000;

#[test]
fn select_only_prod_k1_matches_python_output() {
    let args = SelectOnlyArgs::default();
    let actual = select_only(
        1,
        0,
        TEXT_WORDS_DEFAULT,
        TEXT_WORDS_DEFAULT,
        RAM_WORDS_DEFAULT,
        HWM_DEFAULT,
        &args,
    );
    assert_matches_fixture(
        &actual,
        fixture!("select_only_prod_k1"),
        "select_only_prod_k1",
    );
}

#[test]
fn select_only_small_k2_matches_python_output() {
    let args = SelectOnlyArgs::default();
    let actual = select_only(2, 0, 8, 8, 8, 10_000, &args);
    assert_matches_fixture(
        &actual,
        fixture!("select_only_small_k2"),
        "select_only_small_k2",
    );
}

#[test]
fn select_only_overrides_matches_python_output() {
    let regs0: Vec<u32> = (1..32).collect();
    let args = SelectOnlyArgs {
        pc0: Some(0x8000_1000),
        regs0: Some(&regs0),
        db: "other_db",
        icount0: 12345,
        keyq0: 7,
        ipms: 5_000,
        wl0: "tuple([1,2,3], [10,20,30], [100,200,300])",
    };
    let actual = select_only(
        4096,
        0,
        TEXT_WORDS_DEFAULT,
        TEXT_WORDS_DEFAULT,
        RAM_WORDS_DEFAULT,
        15_000,
        &args,
    );
    assert_matches_fixture(
        &actual,
        fixture!("select_only_overrides"),
        "select_only_overrides",
    );
}

#[test]
fn batch_prod_matches_python_output() {
    let args = BatchArgs {
        db: "clickdoom_executor",
        ipms: 10_000,
    };
    let actual = batch(
        60_000,
        0,
        TEXT_WORDS_DEFAULT,
        TEXT_WORDS_DEFAULT,
        RAM_WORDS_DEFAULT,
        HWM_DEFAULT,
        &args,
    );
    assert_matches_fixture(&actual, fixture!("batch_prod"), "batch_prod");
}

#[test]
fn halt_reason_transform_matches_python_output() {
    assert_matches_fixture(
        &halt_reason_transform("r.4.3"),
        fixture!("halt_reason_transform"),
        "halt_reason_transform",
    );
}
