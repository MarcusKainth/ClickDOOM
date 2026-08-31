//! MMIO width gating, executed against a real ClickHouse server.
//!
//! A non-word-width access at one of the five MMIO register addresses reads
//! 0 for a load and is silently ignored for a store, exactly like an
//! address in the window that names no register. It never reaches the
//! register's own semantics: TICKS_MS's clock, KEYQ's pop, EXIT's halt,
//! PUTCHAR's console push, FRAME_COMMIT's frame commit.
//!
//! These cases live apart from `fold_live.rs` because the reference model
//! carries no MMIO at all, so there is no independent expected value to
//! compare against. Each case asserts on the fold's own output instead, and
//! comes in a pair: the narrow access that must not reach register
//! semantics, and the same address at word width, which must still work.
//! The word case is what shows a fix did not overcorrect into breaking the
//! path the ROM actually uses.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.

#[cfg(feature = "clickhouse-tests")]
mod support;

#[cfg(feature = "clickhouse-tests")]
mod live {
    use clickdoom_executor::config::HALT_EXIT;
    use clickdoom_spec::{IPMS_DEFAULT, MMIO_BASE, mmio};

    use super::support::RAM_BASE;
    use super::support::fixture::Fixture;
    use super::support::fold_case::{FoldCase, FoldRow, run_raw};
    use super::support::insn::{BYTE, WORD, addi, load, store};

    /// The starting icount every TICKS_MS case runs from. Chosen so the
    /// clock reads non-zero: a case that only ever saw TICKS_MS = 0 could
    /// pass because a byte load happened to equal a word value nobody
    /// checked, rather than because the width gate held.
    const TICKS_ICOUNT0: u64 = 53_000;
    const EXPECTED_TICKS_MS: u32 = (TICKS_ICOUNT0 / IPMS_DEFAULT as u64) as u32;
    const _: () = assert!(
        EXPECTED_TICKS_MS != 0,
        "the clock has to read non-zero for these cases to mean anything"
    );

    /// Every store case seeds its register with a leading `addi` rather
    /// than through `select_only`'s `regs0`. A literal array of small
    /// values, which an exit code, a console byte and a frame number all
    /// are, infers as `Array(UInt8)`, and the accumulator's real
    /// `Array(UInt32)` then rejects the whole query.
    async fn run(fx: &Fixture, insns: &[super::support::reference::Insn], k: u32) -> FoldRow {
        run_raw(
            fx,
            &FoldCase {
                insns,
                k: Some(k),
                ..Default::default()
            },
        )
        .await
    }

    #[tokio::test]
    async fn byte_load_at_ticks_ms_reads_zero_not_the_clock() {
        let fx = Fixture::create("byte_load_at_ticks_ms_reads_zero").await;
        let insns = [load(1, MMIO_BASE + mmio::TICKS_MS, BYTE, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                icount0: TICKS_ICOUNT0,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), 0, "a byte load must not read the clock");
        assert_eq!((row.halted, row.retired), (0, 1));
        fx.finish().await;
    }

    #[tokio::test]
    async fn word_load_at_ticks_ms_still_reads_the_clock() {
        let fx = Fixture::create("word_load_at_ticks_ms_reads_the_clock").await;
        let insns = [load(1, MMIO_BASE + mmio::TICKS_MS, WORD, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                icount0: TICKS_ICOUNT0,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), EXPECTED_TICKS_MS);
        fx.finish().await;
    }

    #[tokio::test]
    async fn byte_store_to_exit_does_not_halt() {
        let fx = Fixture::create("byte_store_to_exit_does_not_halt").await;
        let insns = [addi(1, 42), store(1, MMIO_BASE + mmio::EXIT, BYTE)];
        let row = run(&fx, &insns, 2).await;
        assert_eq!(row.halted, 0);
        assert_eq!(row.retired, 2);
        assert_eq!(row.pc, RAM_BASE + 8, "advanced past both, not frozen");
        fx.finish().await;
    }

    #[tokio::test]
    async fn word_store_to_exit_still_halts() {
        let fx = Fixture::create("word_store_to_exit_still_halts").await;
        let insns = [addi(1, 42), store(1, MMIO_BASE + mmio::EXIT, WORD)];
        let row = run(&fx, &insns, 2).await;
        assert_eq!(row.halted, 1);
        assert_eq!(row.halt_reason, HALT_EXIT);
        assert_eq!(row.halt_extra, 42, "the exit code is the stored value");
        assert_eq!(
            row.halt_pc,
            RAM_BASE + 4,
            "frozen at the store, not the addi"
        );
        assert_eq!(row.pc, RAM_BASE + 4, "pc did not advance past it");
        fx.finish().await;
    }

    #[tokio::test]
    async fn byte_store_to_putchar_does_not_push_console_byte() {
        let fx = Fixture::create("byte_store_to_putchar_pushes_nothing").await;
        let insns = [
            addi(1, u32::from(b'Q')),
            store(1, MMIO_BASE + mmio::PUTCHAR, BYTE),
        ];
        let row = run(&fx, &insns, 2).await;
        assert!(row.console_bytes.is_empty());
        assert_eq!((row.halted, row.retired), (0, 2));
        fx.finish().await;
    }

    #[tokio::test]
    async fn word_store_to_putchar_still_pushes_console_byte() {
        let fx = Fixture::create("word_store_to_putchar_pushes_a_byte").await;
        let insns = [
            addi(1, u32::from(b'Q')),
            store(1, MMIO_BASE + mmio::PUTCHAR, WORD),
        ];
        let row = run(&fx, &insns, 2).await;
        assert_eq!(row.console_bytes, vec![b'Q']);
        fx.finish().await;
    }

    #[tokio::test]
    async fn byte_store_to_frame_commit_does_not_commit_a_frame() {
        let fx = Fixture::create("byte_store_to_frame_commit_commits_nothing").await;
        let insns = [addi(1, 7), store(1, MMIO_BASE + mmio::FRAME_COMMIT, BYTE)];
        let row = run(&fx, &insns, 2).await;
        assert_eq!(row.frame_committed, 0);
        assert_eq!((row.halted, row.retired), (0, 2));
        fx.finish().await;
    }

    #[tokio::test]
    async fn word_store_to_frame_commit_still_commits_a_frame() {
        let fx = Fixture::create("word_store_to_frame_commit_commits").await;
        let insns = [addi(1, 7), store(1, MMIO_BASE + mmio::FRAME_COMMIT, WORD)];
        let row = run(&fx, &insns, 2).await;
        assert_eq!(row.frame_committed, 1);
        assert_eq!(row.frame_no, 7, "the stored value is the frame number");
        fx.finish().await;
    }

    #[tokio::test]
    async fn frame_commit_stops_the_batch_without_halting() {
        let fx = Fixture::create("frame_commit_stops_the_batch").await;
        // A batch ends early on a FRAME_COMMIT write, so recording the
        // commit is not enough: the fold has to stop there. K is one more
        // than the commit point, and the third instruction must not run.
        let insns = [
            addi(1, 7),
            store(1, MMIO_BASE + mmio::FRAME_COMMIT, WORD),
            addi(2, 99),
        ];
        let row = run(&fx, &insns, 3).await;
        assert_eq!(row.frame_committed, 1);
        assert_eq!(row.frame_no, 7);
        assert_eq!((row.stopped, row.halted), (1, 0), "stopped, not faulted");
        assert_eq!(
            row.retired, 2,
            "the addi and the commit store, nothing after"
        );
        assert_eq!(row.x(2), 0, "the third instruction was never reached");
        fx.finish().await;
    }

    #[tokio::test]
    async fn byte_load_at_keyq_does_not_pop_the_queue() {
        let fx = Fixture::create("byte_load_at_keyq_pops_nothing").await;
        let insns = [load(1, MMIO_BASE + mmio::KEYQ, BYTE, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                keyq_events: &[0x1234],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), 0, "not the queued event's low byte");
        assert_eq!(row.keyq_pos, 0, "the event is still queued");
        fx.finish().await;
    }

    #[tokio::test]
    async fn word_load_at_keyq_still_pops_the_queue() {
        let fx = Fixture::create("word_load_at_keyq_pops").await;
        let insns = [load(1, MMIO_BASE + mmio::KEYQ, WORD, 0)];
        let row = run_raw(
            &fx,
            &FoldCase {
                insns: &insns,
                keyq_events: &[0x1234],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(1), 0x1234);
        assert_eq!(row.keyq_pos, 1);
        fx.finish().await;
    }
}
