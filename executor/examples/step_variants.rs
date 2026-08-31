//! Emits one step-variant arm as runnable SQL.
//!
//!     cargo run --example step_variants -- <variant> <kind> [K]
//!
//! `<variant>` is a [`Variant`] name in kebab case. `<kind>` is one of:
//!
//! - `batch`, the production INSERT the arm replaces.
//! - `select`, the same fold as a SELECT that writes nothing, for reading
//!   counters off the arm without committing a batch.
//! - `explain`, an `EXPLAIN json = 1, actions = 1` probe over the step as
//!   written, which prints the outer `(acc, i)` lambda's ActionsDAG.
//! - `explain-flat`, the same probe over the step with every lambda binding
//!   replaced by a free column, which prints one DAG holding every action
//!   node the step needs.
//! - `explain-peel`, the same probe with the K argument read as how many
//!   outermost bindings to leave free, which prints one nested scope's DAG.
//! - `step-in`, the `CREATE TABLE` both probes bind their free columns to.
//!
//! Both probes bind `acc` to a column of a `step_in` table instead of to
//! `arrayFold`'s accumulator, which is the only way to get the planner to
//! print a DAG for a lambda body.
//!
//! The shape arguments match `fold_golden.rs`'s `batch_prod` case, so the
//! `baseline` arm's `batch` output is byte-identical to
//! `tests/golden/batch_prod.sql` apart from the K the caller asks for.

use clickdoom_executor::fold::{
    BatchArgs, FLAT_COLUMNS, SelectOnlyArgs, Variant, batch_variant, build_step_flat,
    build_step_peeled, build_step_variant, decode_with, select_only_variant,
};

const TEXT_START_WIDX: u32 = 0;
const TEXT_WORDS: u32 = 524_288;
const RAM_WORDS: u32 = 6_291_456;
const HWM: u32 = 20_000;
const IPMS: u32 = 10_000;
const DB: &str = "clickdoom_executor";
const K_DEFAULT: u32 = 60_000;

fn variant_by_name(name: &str) -> Variant {
    match name {
        "baseline" => Variant::Baseline,
        "inline-halt-code" => Variant::InlineHaltCode,
        "short-binding-param" => Variant::ShortBindingParam,
        "bind-repeated" => Variant::BindRepeated,
        "fewer-constants" => Variant::FewerConstants,
        "more-constants" => Variant::MoreConstants,
        other => panic!(
            "unknown variant {other:?}: expected one of baseline, inline-halt-code, short-binding-param, bind-repeated, fewer-constants, more-constants"
        ),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let variant = variant_by_name(args.first().map(String::as_str).unwrap_or("baseline"));
    let kind = args.get(1).map(String::as_str).unwrap_or("batch");
    let k: u32 = match args.get(2) {
        Some(text) => text.parse().expect("K must be a number"),
        None => K_DEFAULT,
    };

    let sql = match kind {
        "batch" => batch_variant(
            k,
            TEXT_START_WIDX,
            TEXT_WORDS,
            TEXT_WORDS,
            RAM_WORDS,
            HWM,
            &BatchArgs { db: DB, ipms: IPMS },
            variant,
        ),
        "select" => select_only_variant(
            k,
            TEXT_START_WIDX,
            TEXT_WORDS,
            TEXT_WORDS,
            RAM_WORDS,
            HWM,
            &SelectOnlyArgs {
                db: DB,
                ipms: IPMS,
                ..Default::default()
            },
            variant,
        ),
        "explain" | "explain-flat" | "explain-peel" => {
            let step = match kind {
                "explain" => build_step_variant(
                    TEXT_START_WIDX,
                    TEXT_WORDS,
                    TEXT_WORDS,
                    RAM_WORDS,
                    clickdoom_spec::RAM_BASE,
                    HWM,
                    IPMS,
                    variant,
                ),
                "explain-flat" => build_step_flat(
                    TEXT_START_WIDX,
                    TEXT_WORDS,
                    TEXT_WORDS,
                    RAM_WORDS,
                    clickdoom_spec::RAM_BASE,
                    HWM,
                    IPMS,
                    variant,
                ),
                _ => build_step_peeled(
                    TEXT_START_WIDX,
                    TEXT_WORDS,
                    TEXT_WORDS,
                    RAM_WORDS,
                    clickdoom_spec::RAM_BASE,
                    HWM,
                    IPMS,
                    variant,
                    k as usize,
                ),
            };
            format!(
                "EXPLAIN json = 1, actions = 1\nWITH{}\nSELECT {step}\nFROM {DB}.step_in\n\
                 SETTINGS optimize_functions_to_subcolumns = 0, max_threads = 1,\n         \
                 max_ast_elements = 500000, max_expanded_ast_elements = 500000,\n         \
                 max_query_size = 4000000",
                decode_with(DB)
            )
        }
        "step-in" => {
            let columns: Vec<String> = FLAT_COLUMNS
                .iter()
                .map(|(name, ty)| format!("{name} {ty}"))
                .collect();
            format!(
                "CREATE TABLE IF NOT EXISTS {DB}.step_in (\n  \
                 acc Tuple(UInt32, Array(UInt32),\n            \
                 Tuple(Array(UInt32), Array(UInt32), Array(UInt64)),\n            \
                 Tuple(UInt8, UInt8, UInt8, UInt32, UInt32), UInt64,\n            \
                 Tuple(Array(UInt8), UInt32, Tuple(UInt32, UInt8)),\n            \
                 Tuple(Array(UInt32), Array(UInt32), Array(UInt64),\n                  \
                 Array(UInt32), Array(UInt32), Array(UInt64))),\n  \
                 i UInt64, {}) ENGINE = MergeTree ORDER BY tuple()",
                columns.join(", ")
            )
        }
        other => panic!(
            "unknown kind {other:?}: expected batch, select, explain, explain-flat, explain-peel or step-in"
        ),
    };
    println!("{sql}");
}
