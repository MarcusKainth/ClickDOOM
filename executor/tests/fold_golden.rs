//! Byte-for-byte comparison of `fold`'s generated SQL text against the
//! Python original's output for the same inputs.

use clickdoom_executor::config::{K_DEFAULT, LOG_QUERIES_CUT_TO_LENGTH};
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
    let regs0: Vec<String> = (1..32u32).map(|n| n.to_string()).collect();
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

/// The one expression in this project that every feature grows. Past
/// `LOG_QUERIES_CUT_TO_LENGTH` the server keeps a prefix of it in
/// `system.query_log.query` and says nothing, so a query-log capture of a
/// long run would start recording partial statements mid-run and read
/// exactly like one that did not.
///
/// K is not what scales it: K reaches the text only as `range(K)`'s decimal
/// literal, one byte per digit. What scales it is the step expression's node
/// count, which is to say every arm any change adds.
#[test]
fn the_batch_statement_fits_in_the_query_log() {
    let args = BatchArgs {
        db: "clickdoom_executor",
        ipms: 10_000,
    };
    let sql = batch(
        K_DEFAULT,
        0,
        TEXT_WORDS_DEFAULT,
        TEXT_WORDS_DEFAULT,
        RAM_WORDS_DEFAULT,
        HWM_DEFAULT,
        &args,
    );
    let cap = LOG_QUERIES_CUT_TO_LENGTH;
    println!(
        "batch statement: {} bytes, {} of the {cap}-byte query-log cap, {} bytes of headroom",
        sql.len(),
        format_args!("{:.0}%", sql.len() as f64 / cap as f64 * 100.0),
        cap - sql.len().min(cap),
    );
    assert!(
        sql.len() < cap,
        "the batch statement is {} bytes, at or past the {cap}-byte query-log cap: \
         system.query_log would keep a prefix of it and report nothing",
        sql.len()
    );
    // K only reaches the text as a decimal literal, so a K sweep cannot be
    // what crosses the cap. Pinned because the opposite is the intuitive
    // reading and a sweep would otherwise look like the risk.
    let wide_k = batch(
        1_000_000,
        0,
        TEXT_WORDS_DEFAULT,
        TEXT_WORDS_DEFAULT,
        RAM_WORDS_DEFAULT,
        HWM_DEFAULT,
        &args,
    );
    assert_eq!(
        wide_k.len() - sql.len(),
        1_000_000u32.to_string().len() - K_DEFAULT.to_string().len(),
        "K reaches the statement somewhere other than range(K)'s literal"
    );
}

#[test]
fn halt_reason_transform_matches_python_output() {
    assert_matches_fixture(
        &halt_reason_transform("r.4.3"),
        fixture!("halt_reason_transform"),
        "halt_reason_transform",
    );
}
