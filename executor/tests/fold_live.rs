//! The batch fold, executed against a real ClickHouse server over
//! hand-built instruction streams covering every arm and every halt reason.
//!
//! `fold_golden.rs` proves the generated SQL text is byte-identical to a
//! known-correct reference; it never runs a query. This runs the fold for
//! real and checks what it computes, cross-checked against the independent
//! model in `support/reference.rs` wherever that model has an answer. The
//! cases whose addresses land in MMIO, FRAMEBUFFER or PALETTE assert on the
//! fold's own output, since the model carries no region but RAM.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
//!
//! The two assertions on the `wl0` seam's generated text need no server and
//! run unconditionally.

mod support;

use clickdoom_executor::fold::{self, BatchArgs, SelectOnlyArgs};
use clickdoom_executor::word::Widx;
use support::seed::{self, Shape};

/// The production shape the `wl0` text assertions are taken against:
/// K, text window, decode table size, RAM words, high-water mark.
const PROD: (u32, Widx, Widx, u32, u32, u32) = (
    60_000,
    Widx::new(0),
    Widx::new(98_824),
    98_824,
    6_291_456,
    20_000,
);

fn prod_select_only(wl0: &str) -> String {
    let (k, text_start, text_end, decn, ram_words, hwm) = PROD;
    fold::select_only(
        k,
        text_start,
        text_end,
        decn,
        ram_words,
        hwm,
        &SelectOnlyArgs {
            wl0,
            ..Default::default()
        },
    )
}

/// The seam has to be invisible when unused: the default seed is the text
/// `batch` hardcodes, so every caller that does not reach for `wl0` emits
/// the same SQL `batch` does.
#[test]
fn wl0_default_is_byte_identical_to_the_previous_seed() {
    let (k, text_start, text_end, decn, ram_words, hwm) = PROD;
    let default = fold::select_only(
        k,
        text_start,
        text_end,
        decn,
        ram_words,
        hwm,
        &SelectOnlyArgs::default(),
    );
    assert_eq!(default, prod_select_only(fold::WL0_EMPTY));
    assert!(default.contains(fold::WL0_EMPTY));

    let batch = fold::batch(
        k,
        text_start,
        text_end,
        decn,
        ram_words,
        hwm,
        &BatchArgs::default(),
    );
    assert!(batch.contains(fold::WL0_EMPTY));
    for sql in [&default, &batch] {
        assert!(
            !sql.contains("{wl0}"),
            "the seed placeholder reached the SQL"
        );
    }
}

/// The property that makes the seam safe. A per-call-varying literal inside
/// the lambda body is part of ClickHouse's compiled-expression cache key,
/// so it would defeat the JIT. The initial accumulator is `arrayFold`'s
/// runtime argument, evaluated in the outer `SELECT`, so a seed there
/// cannot. Asserted rather than assumed: the step expression appears
/// byte-identically in a seeded query, and a seeded query differs from the
/// default by exactly the seed text.
#[test]
fn wl0_never_reaches_the_lambda() {
    let (_, text_start, text_end, decn, ram_words, hwm) = PROD;
    let step = fold::build_step(
        text_start,
        text_end,
        decn,
        ram_words,
        clickdoom_spec::RAM_BASE,
        hwm,
        clickdoom_spec::IPMS_DEFAULT,
    );
    let seed_sql = seed::seed_sql(Shape::AllLanes, 80_000);
    let seeded = prod_select_only(&seed_sql);
    assert!(seeded.contains(&step));

    let default = prod_select_only(fold::WL0_EMPTY);
    assert_eq!(
        seeded.len() - default.len(),
        seed_sql.len() - fold::WL0_EMPTY.len(),
        "a seeded query differs from the default outside the initial accumulator"
    );
}

#[cfg(feature = "clickhouse-tests")]
mod live {
    use std::collections::BTreeSet;

    use clickdoom_executor::config::{
        HALT_EXIT, HALT_REASON_NAMES, LOG_QUERIES_CUT_TO_LENGTH, OP_ILLEGAL, RAM_WORDS_DEFAULT,
    };
    use clickdoom_executor::fold;
    use clickdoom_spec::{FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, MMIO_BASE, PALETTE_BASE, mmio};

    use super::seed::{self, Shape};
    use super::support::RAM_BASE;
    use super::support::fixture::Fixture;
    use super::support::fold_case::{FoldCase, run_checked, run_checked_labelled, run_raw};
    use super::support::insn::{BYTE, HALF, WORD, addi, alu, bare, branch, jal, load, store};
    use super::support::reference::{
        HALT_BAD_ADDR, HALT_CSR, HALT_EBREAK, HALT_ECALL, HALT_ILLEGAL_INSN, HALT_MISALIGNED,
        HALT_NONE, HALT_SELF_MODIFY, Insn, OP_CSR, OP_EBREAK, OP_ECALL,
    };

    #[tokio::test]
    async fn alu_arms_straight_line() {
        let fx = Fixture::create("alu_arms_straight_line").await;
        // Every ALU arm from sub to remu, each writing its own register so
        // every arm's result is compared and not only the last one's.
        //
        // The second operand pair is what separates the signed arms from
        // their unsigned twins: with two small positive operands, slt and
        // sltu agree, srl and sra agree, and so do div/divu and rem/remu,
        // so a query answering all of them the same way would still pass.
        for (a, b) in [(5u32, 3u32), (-3i32 as u32, 5u32)] {
            let mut insns = vec![addi(1, a), addi(2, b)];
            insns.extend((1..=17).map(|op_id| alu(op_id, 2 + op_id as u8, 1, 2)));
            run_checked_labelled(
                &fx,
                &FoldCase {
                    insns: &insns,
                    ..Default::default()
                },
                &format!("a={a:#x} b={b:#x}"),
            )
            .await;
        }
        fx.finish().await;
    }

    #[tokio::test]
    async fn div_by_zero_and_x0_discard() {
        let fx = Fixture::create("div_by_zero_and_x0_discard").await;
        let insns = [
            addi(1, 7),
            // div by x0 into x0: the result must be discarded.
            alu(14, 0, 1, 0),
            // divu by zero: 0xFFFFFFFF.
            alu(15, 2, 1, 0),
        ];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), 7, "x1 must survive the discarded write to x0");
        assert_eq!(row.x(2), 0xFFFF_FFFF);
        fx.finish().await;
    }

    #[tokio::test]
    async fn store_then_load_shadows_ram() {
        let fx = Fixture::create("store_then_load_shadows_ram").await;
        // The decode table pads to 8 words, so words 0..7 are text. Word 8
        // (byte offset 32) is outside it, which makes this the write-log
        // path rather than a SELF_MODIFY halt.
        let insns = [
            addi(1, 100),
            store(1, RAM_BASE + 32, WORD),
            load(2, RAM_BASE + 32, WORD, 0),
        ];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ram: &[(8, 999)],
                ram_words: Some(16),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            row.x(2),
            100,
            "the write-log must shadow the stale RAM word"
        );
        assert_eq!(
            row.wl_icount,
            vec![2],
            "the store's own icount, not the batch's"
        );
        fx.finish().await;
    }

    #[tokio::test]
    async fn load_byte_sign_extend() {
        let fx = Fixture::create("load_byte_sign_extend").await;
        let insns = [load(1, RAM_BASE, BYTE, 1)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ram: &[(0, 0xFFFF_FF80)],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), 0xFFFF_FF80, "low byte 0x80 sign-extends to -128");
        fx.finish().await;
    }

    #[tokio::test]
    async fn branches_and_jumps() {
        let fx = Fixture::create("branches_and_jumps").await;
        // The target is a byte address: word 7 of the decode table.
        let target = RAM_BASE + 7 * 4;
        let cases: [(u32, u32, u32, bool); 8] = [
            (20, 5, 5, true),
            (20, 5, 6, false),
            (21, 5, 6, true),
            (21, 5, 5, false),
            (22, -1i32 as u32, 1, true),
            (23, -1i32 as u32, 1, false),
            (24, 1, 0xFFFF_FFFF, true),
            (25, 1, 0xFFFF_FFFF, false),
        ];
        for (op_id, a, b, taken) in cases {
            let insns = [
                addi(1, a),
                addi(2, b),
                branch(op_id, 1, 2, target),
                // Fallthrough marker.
                addi(5, 111),
            ];
            let label = format!("op_id={op_id} a={a:#x} b={b:#x}");
            let row = run_checked_labelled(
                &fx,
                &FoldCase {
                    insns: &insns,
                    k: Some(4),
                    ..Default::default()
                },
                &label,
            )
            .await;
            assert_eq!(row.pc == target, taken, "{label}");
        }
        fx.finish().await;
    }

    #[tokio::test]
    async fn jal_jalr() {
        let fx = Fixture::create("jal_jalr").await;
        // The jump target and the link value are independent. The target is
        // deliberately not RAM_BASE + 4, which is what the link value is,
        // so reading one for the other fails loudly.
        let target = RAM_BASE + 99 * 4;
        let insns = [jal(1, target), addi(9, 111)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.pc, target, "pc takes the decoded target, unclamped");
        assert_eq!(row.x(1), RAM_BASE + 4, "x1 takes the link value, pc + 4");
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_ecall_ebreak_csr_illegal() {
        let fx = Fixture::create("halt_ecall_ebreak_csr_illegal").await;
        for (op_id, reason) in [
            (OP_ECALL, HALT_ECALL),
            (OP_EBREAK, HALT_EBREAK),
            (OP_CSR, HALT_CSR),
        ] {
            let insns = [addi(1, 1), bare(op_id)];
            let label = format!("op_id={op_id}");
            let row = run_checked_labelled(
                &fx,
                &FoldCase {
                    insns: &insns,
                    k: Some(2),
                    ..Default::default()
                },
                &label,
            )
            .await;
            assert_eq!((row.halted, row.halt_reason), (1, reason), "{label}");
            assert_eq!(row.halt_pc, RAM_BASE + 4, "frozen at the faulting word");
            assert_eq!(row.pc, RAM_BASE + 4, "pc did not advance past it");
            assert_eq!(row.x(1), 1, "the prior instruction still retired");
        }
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_illegal_carries_raw_word() {
        let fx = Fixture::create("halt_illegal_carries_raw_word").await;
        // The padding rows are ILLEGAL, carrying a raw word derived from
        // their own index, so the second step lands on one.
        let insns = [addi(1, 1)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_ILLEGAL_INSN));
        assert_eq!(row.halt_extra, 0xBAD0_0001);
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_bad_addr() {
        let fx = Fixture::create("halt_bad_addr").await;
        // One word below RAM.
        let insns = [load(1, RAM_BASE.wrapping_sub(4), WORD, 0)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ram_words: Some(8),
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_BAD_ADDR));
        assert_eq!(row.halt_extra, RAM_BASE.wrapping_sub(4));
        assert_eq!(row.x(1), 0, "the load did not retire");
        fx.finish().await;
    }

    /// Both §1's misaligned rule and §2's out-of-region rule fire for this
    /// address, and the contract says misalignment wins. It is the case the
    /// two engines disagreed on: the fold answered `BAD_ADDR` from arm
    /// order alone while `refemu` decided alignment before the region.
    #[tokio::test]
    async fn a_misaligned_access_outside_every_region_halts_misaligned() {
        let fx = Fixture::create("misaligned_outside_every_region").await;
        // Address 1 is in no region, and misaligned for a word access.
        let insns = [load(1, 1, WORD, 0)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ram_words: Some(8),
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        assert_eq!(row.halt_extra, 1, "the faulting address");
        assert_eq!(row.x(1), 0, "the load did not retire");
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_misaligned() {
        let fx = Fixture::create("halt_misaligned").await;
        // RAM_BASE + 2 is a word load off a word boundary.
        let insns = [load(1, RAM_BASE + 2, WORD, 0)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_self_modify() {
        let fx = Fixture::create("halt_self_modify").await;
        // Word 0 is inside the text window.
        let insns = [store(0, RAM_BASE, WORD)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_SELF_MODIFY));
        assert!(row.wl_addr.is_empty(), "the faulting store never retired");
        fx.finish().await;
    }

    #[tokio::test]
    async fn high_water_mark_stops_without_halting() {
        let fx = Fixture::create("high_water_mark_stops_without_halting").await;
        // Words 0..7 are text, so store from word 8 up: this exercises the
        // high-water mark, not SELF_MODIFY.
        let insns: Vec<Insn> = (0..6)
            .map(|i| store(0, RAM_BASE + (8 + i) * 4, WORD))
            .collect();
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                ram_words: Some(16),
                hwm: 3,
                k: Some(6),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.stopped, row.halted), (1, 0));
        assert_eq!(row.wl_addr.len(), 3);
        assert_eq!(row.retired, 3);
        assert_eq!(row.wl_icount, vec![1, 2, 3]);
        fx.finish().await;
    }

    #[tokio::test]
    async fn stopped_step_is_a_no_op() {
        let fx = Fixture::create("stopped_step_is_a_no_op").await;
        let insns = [bare(OP_ECALL), addi(1, 42)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            row.x(1),
            0,
            "the instruction after the halt is never reached"
        );
        assert_eq!(row.retired, 0);
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_jal_misaligned_target() {
        let fx = Fixture::create("halt_jal_misaligned_target").await;
        // A well-formed RV32IM binary cannot produce this: jump and branch
        // encodings force bit 0, and no toolchain emits a target with bit 1
        // set. Nothing else exercises the path, so this is what keeps the
        // three engines' agreement on it real rather than assumed.
        let target = RAM_BASE + 4 + 2;
        let insns = [jal(1, target), addi(1, 111)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        assert_eq!(row.halt_extra, target);
        assert_eq!(row.halt_pc, RAM_BASE, "the jal's own pc, not the target");
        assert_eq!(row.pc, RAM_BASE, "pc did not complete onto the target");
        assert_eq!(row.x(1), 0, "rd did not take the link value");
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_jalr_misaligned_target() {
        let fx = Fixture::create("halt_jalr_misaligned_target").await;
        let target_base = RAM_BASE + 4 + 2;
        let insns = [
            addi(1, target_base),
            Insn {
                op_id: 27,
                rd: 2,
                rs1: 1,
                ..Insn::default()
            },
        ];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        assert_eq!(
            row.halt_extra, target_base,
            "jalr clears bit 0 only, so bit 1 survives"
        );
        assert_eq!(row.halt_pc, RAM_BASE + 4, "the jalr's own pc");
        assert_eq!(row.x(2), 0, "rd did not take the link value");
        fx.finish().await;
    }

    #[tokio::test]
    async fn branch_misaligned_target_only_halts_if_taken() {
        let fx = Fixture::create("branch_misaligned_target_only_halts_if_taken").await;
        let target = RAM_BASE + 4 + 2;

        // Not taken: the misaligned target is never used, so the check has
        // to be gated on the branch being taken rather than applied blanket.
        let insns = [
            addi(1, 5),
            addi(2, 6),
            branch(20, 1, 2, target),
            addi(3, 222),
        ];
        let row = run_checked_labelled(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(4),
                ..Default::default()
            },
            "not taken",
        )
        .await;
        assert_eq!(row.halted, 0);
        assert_eq!(row.x(3), 222, "the fallthrough ran normally");

        // Taken: the same instructions with equal operands must halt.
        let insns = [
            addi(1, 5),
            addi(2, 5),
            branch(20, 1, 2, target),
            addi(3, 222),
        ];
        let row = run_checked_labelled(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(4),
                ..Default::default()
            },
            "taken",
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        assert_eq!(row.halt_extra, target);
        fx.finish().await;
    }

    #[tokio::test]
    async fn div_rem_int_min_by_minus_one_does_not_trap() {
        let fx = Fixture::create("div_rem_int_min_by_minus_one_does_not_trap").await;
        // RV32IM defines DIV of INT_MIN by -1 as INT_MIN and REM as 0.
        // ClickHouse's 32-bit intDiv and modulo raise ILLEGAL_DIVISION on
        // exactly that pair, so the fold does the division in 64 bits. A
        // regression shows up as the query failing outright, before any
        // assertion below is reached.
        //
        // DIVU and REMU are here for contrast: unsigned division of
        // 0x80000000 by 0xFFFFFFFF has no overflow to escape.
        let insns = [
            addi(1, i32::MIN as u32),
            addi(2, -1i32 as u32),
            alu(14, 3, 1, 2),
            alu(16, 4, 1, 2),
            alu(15, 5, 1, 2),
            alu(17, 6, 1, 2),
        ];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(6),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.halted, 0, "a defined result, not a fault");
        assert_eq!(row.x(3), 0x8000_0000, "div wraps to the same bit pattern");
        assert_eq!(row.x(4), 0, "rem is 0");
        assert_eq!(row.x(5), 0);
        assert_eq!(row.x(6), 0x8000_0000);
        fx.finish().await;
    }

    #[tokio::test]
    async fn framebuffer_word_store_lands_in_its_own_lane() {
        let fx = Fixture::create("framebuffer_word_store_lands_in_its_own_lane").await;
        let insns = [addi(1, 0xDEAD_BEEF), store(1, FRAMEBUFFER_BASE, WORD)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.halted, 0, "a clean word store to FRAMEBUFFER");
        assert!(row.wl_addr.is_empty(), "it must not reach RAM's write-log");
        assert_eq!(row.fb_wl_addr, vec![0]);
        assert_eq!(row.fb_wl_val, vec![0xDEAD_BEEF]);
        assert_eq!(row.fb_wl_icount, vec![2], "the second retiring instruction");
        assert!(row.pal_wl_addr.is_empty());
        fx.finish().await;
    }

    #[tokio::test]
    async fn framebuffer_last_word_is_in_bounds() {
        let fx = Fixture::create("framebuffer_last_word_is_in_bounds").await;
        // The last word-aligned offset inside the region.
        let last_word_addr = FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE - 4;
        let insns = [addi(1, 7), store(1, last_word_addr, WORD)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.halted, 0);
        assert_eq!(row.fb_wl_addr, vec![FRAMEBUFFER_SIZE / 4 - 1]);
        assert_eq!(row.fb_wl_val, vec![7]);
        fx.finish().await;
    }

    #[tokio::test]
    async fn palette_word_store_lands_in_its_own_lane() {
        let fx = Fixture::create("palette_word_store_lands_in_its_own_lane").await;
        let insns = [addi(1, 0x0011_2233), store(1, PALETTE_BASE, WORD)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.halted, 0);
        assert!(row.fb_wl_addr.is_empty());
        assert_eq!(row.pal_wl_addr, vec![0]);
        assert_eq!(row.pal_wl_val, vec![0x0011_2233]);
        assert_eq!(row.pal_wl_icount, vec![2]);
        fx.finish().await;
    }

    #[tokio::test]
    async fn framebuffer_load_halts_bad_addr() {
        let fx = Fixture::create("framebuffer_load_halts_bad_addr").await;
        let insns = [load(1, FRAMEBUFFER_BASE, WORD, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_BAD_ADDR));
        assert_eq!(row.halt_extra, FRAMEBUFFER_BASE);
        assert!(
            row.fb_wl_addr.is_empty(),
            "a halted load must not have touched the write-log"
        );
        fx.finish().await;
    }

    #[tokio::test]
    async fn palette_load_halts_bad_addr() {
        let fx = Fixture::create("palette_load_halts_bad_addr").await;
        let insns = [load(1, PALETTE_BASE, WORD, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_BAD_ADDR));
        fx.finish().await;
    }

    #[tokio::test]
    async fn framebuffer_narrow_store_halts_bad_addr() {
        let fx = Fixture::create("framebuffer_narrow_store_halts_bad_addr").await;
        // Nothing ever reads these lanes, so a sub-word store has no
        // previous word to blend against and no correct answer. It halts
        // rather than dropping or corrupting a pixel.
        let insns = [addi(1, 0x1234), store(1, FRAMEBUFFER_BASE, HALF)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_BAD_ADDR));
        assert_eq!(row.halt_pc, RAM_BASE + 4, "the store's own pc");
        assert!(row.fb_wl_addr.is_empty());
        fx.finish().await;
    }

    /// §1's misaligned rule and §2 clause 2's word-width rule both fire for
    /// this store, and misalignment wins. The aligned case above is the one
    /// clause 2 owns on its own.
    #[tokio::test]
    async fn framebuffer_misaligned_narrow_store_halts_misaligned() {
        let fx = Fixture::create("framebuffer_misaligned_narrow_store").await;
        let insns = [addi(1, 0x1234), store(1, FRAMEBUFFER_BASE + 1, HALF)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_MISALIGNED));
        assert_eq!(row.halt_pc, RAM_BASE + 4, "the store's own pc");
        assert_eq!(row.halt_extra, FRAMEBUFFER_BASE + 1);
        assert!(row.fb_wl_addr.is_empty());
        fx.finish().await;
    }

    #[tokio::test]
    async fn framebuffer_store_does_not_trigger_self_modify() {
        let fx = Fixture::create("framebuffer_store_does_not_trigger_self_modify").await;
        // The RAM-relative word index underflows for a FRAMEBUFFER address,
        // since FRAMEBUFFER sits below RAM_BASE, and clamps to some value.
        // That value must not land inside the text window. The decode table
        // pads to 8 words here, which makes the window as small as it gets
        // and a wrong index as likely as it gets to fall inside it.
        let insns = [addi(1, 1), store(1, FRAMEBUFFER_BASE, WORD)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.halted, 0);
        assert_eq!(row.halt_reason, HALT_NONE);
        fx.finish().await;
    }

    #[tokio::test]
    async fn halt_exit_reachable() {
        let fx = Fixture::create("halt_exit_reachable").await;
        // A word store to the EXIT register is the ROM's clean stop, not a
        // fault: halt_reason is EXIT and halt_extra is the stored value,
        // where BAD_ADDR, MISALIGNED and SELF_MODIFY all put the faulting
        // address.
        let exit_addr = MMIO_BASE + mmio::EXIT;
        let insns = [addi(1, 0xDEAD_BEEF), store(1, exit_addr, WORD)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                ..Default::default()
            },
        )
        .await;
        assert_eq!((row.halted, row.halt_reason), (1, HALT_EXIT));
        assert_eq!(
            row.halt_extra, 0xDEAD_BEEF,
            "halt_extra is the exit code, not an address"
        );
        fx.finish().await;
    }

    #[tokio::test]
    async fn every_halt_reason_is_reachable_somewhere_in_this_file() {
        let fx = Fixture::create("every_halt_reason_is_reachable").await;
        // Every halt code the SQL `transform` can emit has to be producible
        // by some case here. A silently unreachable arm otherwise surfaces
        // only as a divergence from refemu.
        let exit_addr = MMIO_BASE + mmio::EXIT;
        let cases: [(&str, Vec<Insn>, u32); 8] = [
            (
                "illegal",
                vec![Insn {
                    op_id: OP_ILLEGAL,
                    raw: 0xBAD,
                    ..Insn::default()
                }],
                1,
            ),
            (
                "self modify",
                vec![addi(1, 0xDEAD_BEEF), store(1, RAM_BASE, WORD)],
                2,
            ),
            ("bad addr", vec![store(0, 0, WORD)], 1),
            ("misaligned", vec![store(0, RAM_BASE + 1, WORD)], 1),
            ("ecall", vec![bare(OP_ECALL)], 1),
            ("ebreak", vec![bare(OP_EBREAK)], 1),
            ("csr", vec![bare(OP_CSR)], 1),
            ("exit", vec![addi(1, 1), store(1, exit_addr, WORD)], 2),
        ];
        let mut covered: BTreeSet<u8> = BTreeSet::new();
        for (label, insns, k) in cases {
            let row = run_raw(
                &fx,
                &FoldCase {
                    insns: &insns,
                    k: Some(k),
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(row.halted, 1, "{label} did not halt: {row:?}");
            covered.insert(row.halt_reason);
        }
        let expected: BTreeSet<u8> = HALT_REASON_NAMES.iter().map(|(code, _)| *code).collect();
        assert_eq!(
            covered,
            expected,
            "missing halt reasons: {:?}",
            expected.difference(&covered).collect::<Vec<_>>()
        );
        fx.finish().await;
    }

    /// A program that both stores and loads, for the seed cases: RAM starts
    /// at `word * 7` so a load from RAM is distinguishable from a forwarded
    /// one and from a seeded slot.
    fn seed_program() -> Vec<Insn> {
        vec![
            addi(1, 0x1234),
            store(1, RAM_BASE + 32, WORD),
            // Forwarded from the write-log.
            load(2, RAM_BASE + 32, WORD, 0),
            // Read straight from RAM.
            load(3, RAM_BASE + 16, WORD, 0),
            store(2, RAM_BASE + 36, WORD),
        ]
    }

    fn dense_ram(words: u32) -> Vec<(u32, u32)> {
        (0..words).map(|w| (w, w * 7)).collect()
    }

    #[tokio::test]
    async fn wl0_seed_is_inert_when_executed() {
        let fx = Fixture::create("wl0_seed_is_inert_when_executed").await;
        // The seed's inertness is argued structurally in `seed.rs`, from
        // the clamp on a load's word index. An argument that is never
        // executed is indistinguishable from one that is wrong, so run the
        // same program seeded and unseeded and require identical results.
        const L0: usize = 8;
        let insns = seed_program();
        let ram = dense_ram(64);
        let seeded_sql = seed::seed_sql(Shape::AllLanes, L0 as u32);
        let base = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                ram: &ram,
                ram_words: Some(64),
                wl0: fold::WL0_EMPTY,
                ..Default::default()
            },
        )
        .await;
        let got = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                ram: &ram,
                ram_words: Some(64),
                wl0: &seeded_sql,
                ..Default::default()
            },
        )
        .await;

        // Architectural state is untouched.
        assert_eq!(got.pc, base.pc);
        assert_eq!(got.regs, base.regs);
        assert_eq!(
            (got.stopped, got.halted, got.halt_reason),
            (base.stopped, base.halted, base.halt_reason)
        );
        assert_eq!(
            (got.halt_pc, got.halt_extra),
            (base.halt_pc, base.halt_extra)
        );
        assert_eq!(got.retired, base.retired);
        assert_eq!(got.keyq_pos, base.keyq_pos);
        assert_eq!(
            (got.frame_no, got.frame_committed),
            (base.frame_no, base.frame_committed)
        );

        // The load that forwards still forwards.
        assert_eq!(got.x(2), 0x1234, "a forwarded load broke under a seed");

        // Every lane is its seeded prefix followed by exactly the real stores.
        assert_eq!(&got.wl_addr[L0..], &base.wl_addr[..]);
        assert_eq!(&got.wl_val[L0..], &base.wl_val[..]);
        assert_eq!(&got.wl_icount[L0..], &base.wl_icount[..]);
        assert!(got.wl_addr[..L0].iter().all(|a| *a == seed::SENTINEL_ADDR));
        assert!(got.wl_val[..L0].iter().all(|v| *v == 0));
        assert!(got.wl_icount[..L0].iter().all(|v| *v == 0));
        fx.finish().await;
    }

    #[tokio::test]
    async fn unequal_lane_seed_breaks_forwarding() {
        let fx = Fixture::create("unequal_lane_seed_breaks_forwarding").await;
        // The three lanes of the write-log are parallel arrays: forwarding
        // finds an index in the address lane and subscripts the value lane
        // with it. Seeding them to different lengths desynchronises them
        // and forwarding silently reads the wrong slot, which is why the
        // seed shapes are limited to equal lengths.
        const L0: u32 = 8;
        let program = seed_program();
        let insns = &program[..3];
        let ram = dense_ram(64);
        let aligned_sql = seed::seed_sql(Shape::AllLanes, L0);
        let aligned = run_raw(
            &fx,
            &FoldCase {
                insns,
                ram: &ram,
                ram_words: Some(64),
                wl0: &aligned_sql,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(aligned.x(2), 0x1234, "an aligned seed still forwards");

        // Address lane seeded, value lane empty: the index found in the
        // first lane overruns the second.
        let lopsided = format!(
            "tuple(arrayResize(emptyArrayUInt32(), {L0}, toUInt32({})), \
             emptyArrayUInt32(), emptyArrayUInt64())",
            seed::SENTINEL_ADDR
        );
        let broken = run_raw(
            &fx,
            &FoldCase {
                insns,
                ram: &ram,
                ram_words: Some(64),
                wl0: &lopsided,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            broken.x(2),
            0,
            "an unequal-lane seed no longer breaks forwarding, so the lanes \
             may have stopped being parallel arrays and seed.rs is stale"
        );
        fx.finish().await;
    }

    /// The sentinel has to sit outside the production RAM window, or a
    /// seeded entry could match a real load's word index. Clippy only sees
    /// today's sentinel, which is the largest `u32` there is; the check is
    /// on whatever the sentinel becomes.
    #[allow(clippy::absurd_extreme_comparisons)]
    const _: () = assert!(seed::SENTINEL_ADDR >= RAM_WORDS_DEFAULT);

    #[tokio::test]
    async fn wl0_seed_never_matches_a_real_load() {
        let fx = Fixture::create("wl0_seed_never_matches_a_real_load").await;
        // The fold clamps every word index to at most ram_words - 1, so the
        // sentinel address is unreachable as a load's index. If that ever
        // stopped holding, a seeded load would forward the seed's 0 instead
        // of RAM's own word.
        let insns = [load(3, RAM_BASE + 16, WORD, 0)];
        let ram = dense_ram(64);
        let seeded_sql = seed::seed_sql(Shape::AllLanes, 4096);
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                ram: &ram,
                ram_words: Some(64),
                k: Some(1),
                wl0: &seeded_sql,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(3), 4 * 7, "the seeded write-log shadowed a real read");
        fx.finish().await;
    }

    /// The cap the batch statement's length is asserted against in
    /// `fold_golden.rs` is the server's own default, and a server profile or
    /// a version bump could move it. Read it back rather than assume it: a
    /// lowered cap would truncate the statement in `system.query_log` with
    /// nothing to say so, and the length assertion would keep passing
    /// against a number the server no longer uses.
    #[tokio::test]
    async fn the_server_agrees_on_the_query_log_cap() {
        let db = super::support::db::Conn::from_env().open("default");
        let value: u64 = db
            .fetch_one(
                "SELECT toUInt64(value) FROM system.settings \
                 WHERE name = 'log_queries_cut_to_length'",
            )
            .await
            .unwrap();
        assert_eq!(
            value, LOG_QUERIES_CUT_TO_LENGTH as u64,
            "the server's log_queries_cut_to_length is not the value \
             clickdoom_executor::config pins"
        );
    }
}
