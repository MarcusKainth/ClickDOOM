//! The register checkpoints the fold records, executed against a real
//! ClickHouse server.
//!
//! The trace cadence is 256x finer than a batch, so a batch crosses
//! boundaries it can never land on, and `arrayFold` exposes no intermediate
//! accumulator. The fold therefore appends `(icount, pc, regs)` at each
//! boundary and commits them with the batch. These cases pin which
//! boundaries end up in that lane and which do not.
//!
//! Every case starts from an `icount0` a few instructions below a boundary
//! rather than shrinking `CHECKPOINT_INTERVAL`, so what runs here is the
//! interval the ROM run uses.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.

#[cfg(feature = "clickhouse-tests")]
mod support;

#[cfg(feature = "clickhouse-tests")]
mod live {
    use clickdoom_executor::fold::CHECKPOINT_REGS;
    use clickdoom_spec::CHECKPOINT_INTERVAL;

    use super::support::RAM_BASE;
    use super::support::fixture::Fixture;
    use super::support::fold_case::{FoldCase, FoldRow, run_checked};
    use super::support::insn::{addi, jal};
    use super::support::reference::Insn;

    const INTERVAL: u64 = CHECKPOINT_INTERVAL;

    /// `add rd, rs1, imm`: the two-source ALU arm with the immediate
    /// standing in for x0's operand, which is how a counter increments.
    fn add_imm(rd: u8, rs1: u8, imm: u32) -> Insn {
        Insn {
            op_id: 0,
            rd,
            rs1,
            imm,
            ..Insn::default()
        }
    }

    /// The `n`-th checkpoint's register file, x1..x31.
    fn checkpoint_regs(row: &FoldRow, n: usize) -> Vec<u32> {
        let width = CHECKPOINT_REGS as usize;
        assert_eq!(
            row.cp_regs.len(),
            row.cp_icount.len() * width,
            "cp_regs holds {width} words per checkpoint"
        );
        row.cp_regs[n * width..(n + 1) * width].to_vec()
    }

    #[tokio::test]
    async fn a_boundary_crossed_inside_the_batch_is_recorded() {
        let fx = Fixture::create("boundary_inside_the_batch_is_recorded").await;
        // Retires to the boundary on the second step, so a third step is
        // left to record it.
        let insns = [addi(1, 7), addi(2, 8), addi(3, 9), addi(4, 10)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(4),
                icount0: INTERVAL - 2,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.cp_icount, vec![INTERVAL]);
        assert_eq!(
            row.cp_pc,
            vec![RAM_BASE + 8],
            "the pc after the boundary instruction retired"
        );
        let mut expected = vec![0u32; CHECKPOINT_REGS as usize];
        expected[0] = 7;
        expected[1] = 8;
        assert_eq!(
            checkpoint_regs(&row, 0),
            expected,
            "x3 and x4 are written after the boundary and must not appear"
        );
        fx.finish().await;
    }

    #[tokio::test]
    async fn a_batch_starting_on_a_boundary_does_not_re_record_it() {
        let fx = Fixture::create("batch_starting_on_a_boundary_records_nothing").await;
        // The batch before this one already recorded the boundary its own
        // last instruction landed on. Recording it again would compare it
        // twice and overshoot the run's comparison count.
        let insns = [addi(1, 7), addi(2, 8)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(2),
                icount0: INTERVAL,
                ..Default::default()
            },
        )
        .await;
        assert!(row.cp_icount.is_empty());
        assert!(row.cp_regs.is_empty());
        fx.finish().await;
    }

    #[tokio::test]
    async fn a_boundary_on_the_batch_s_last_instruction_is_not_recorded() {
        let fx = Fixture::create("boundary_on_the_last_instruction").await;
        // No step follows it, so this one is only observable as the
        // batch's own committed state. The driver reads it from there.
        let insns = [addi(1, 7)];
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(1),
                icount0: INTERVAL - 1,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.retired, 1);
        assert!(row.cp_icount.is_empty());
        fx.finish().await;
    }

    #[tokio::test]
    async fn two_boundaries_in_one_batch_carry_their_own_register_files() {
        let fx = Fixture::create("two_boundaries_in_one_batch").await;
        // A two-instruction loop incrementing x1, run across two
        // boundaries. The register files differ, so a wrong slice into the
        // flat `cp_regs` shows up as the wrong counter value rather than
        // as a shape that still looks plausible.
        let insns = [add_imm(1, 1, 1), jal(0, RAM_BASE)];
        let k = (INTERVAL + 2) as u32;
        let row = run_checked(
            &fx,
            &FoldCase {
                insns: &insns,
                k: Some(k),
                icount0: INTERVAL - 1,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.cp_icount, vec![INTERVAL, 2 * INTERVAL]);
        assert_eq!(
            row.cp_pc,
            vec![RAM_BASE + 4, RAM_BASE + 4],
            "the loop has period two and both boundaries are an even number of steps apart"
        );
        assert_eq!(checkpoint_regs(&row, 0)[0], 1);
        assert_eq!(
            checkpoint_regs(&row, 1)[0],
            1 + (INTERVAL / 2) as u32,
            "x1 counts one increment per two instructions"
        );
        fx.finish().await;
    }
}
