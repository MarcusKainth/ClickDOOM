//! The batch-commit flushes and `fold::batch`'s reshape, executed against a
//! real ClickHouse server.
//!
//! Every test runs against the real `sqlcpu/schema.sql`, renamed onto its
//! own private database, so `batch_commit`'s column shape can never drift
//! from what the engine ships.
//!
//! Two properties get most of the attention here, both of the silent,
//! deterministic, wrong kind:
//!
//!   * `wl_icount` is the store's absolute icount across batches, not its
//!     rank within one. Checked by writing to the same address from
//!     batches whose starting icounts differ widely, and confirming
//!     `ram FINAL` holds the chronologically later write rather than
//!     whichever part merges last.
//!   * A batch commit survives a crash between the `batch_commit` row
//!     landing and the flush running. Checked with the checkpoint trace's
//!     own RAM hash rather than a hand-rolled comparison.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
//!
//! The assertion on `retention_sql`'s emitted text needs no server and runs
//! unconditionally.

#[cfg(feature = "clickhouse-tests")]
mod support;

use clickdoom_executor::commit::retention_sql;
use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;

/// The setting has to be in the emitted SQL, not merely intended: a guard
/// is only real once it is shown carrying what it exists for.
#[test]
fn retention_sql_carries_the_async_setting() {
    let sql = retention_sql("clickdoom", 1, BATCH_COMMIT_RETENTION_N);
    assert!(sql.contains("lightweight_deletes_sync = 0"), "{sql}");
}

#[cfg(feature = "clickhouse-tests")]
mod live {
    use std::time::{Duration, Instant}; // purity-ok: a harness timeout while an async DELETE finishes, off every computation path

    use clickdoom_executor::commit::{
        console_out_flush_sql, cpu_state_flush_sql, fbpal_flush_sql, ram_flush_sql, retention_sql,
    };
    use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;
    use clickdoom_executor::fold::{self, BatchArgs};
    use clickdoom_spec::{FRAMEBUFFER_BASE, PALETTE_BASE};
    use clickhouse::Row;
    use serde::Deserialize;

    use super::support::db::split_statements;
    use super::support::fixture::Fixture;
    use super::support::insn::{WORD, addi, store};
    use super::support::reference::Insn;
    use super::support::{RAM_BASE, RAM_BASE_WORD};

    const HWM: u32 = 20_000;

    /// Clears `decoded` and `ram`, writes `insns` at consecutive word
    /// addresses, and fills `ram` densely with zeros over the window. The
    /// fold captures `ram` as one positionally indexed array, so every word
    /// needs a row.
    async fn seed_decoded_and_ram(fx: &Fixture, insns: &[Insn], ram_words: u32) {
        fx.truncate(&["decoded", "ram"]).await;
        fx.seed_decoded(insns).await;
        fx.seed_ram(&vec![0u32; ram_words as usize]).await;
    }

    async fn run_batch(fx: &Fixture, k: u32, decn: u32, ram_words: u32) {
        let sql = fold::batch(
            k,
            0,
            decn,
            decn,
            ram_words,
            HWM,
            &BatchArgs {
                db: &fx.database,
                ..Default::default()
            },
        );
        fx.db.run(&sql).await.unwrap();
    }

    /// Every flush, in the order the driver runs them, for `batch_id`.
    /// `fbpal_flush_sql` returns two statements in one string, and the HTTP
    /// interface takes one statement per request.
    async fn flush_batch(fx: &Fixture, batch_id: u64) {
        let db = &fx.database;
        fx.db.run(&ram_flush_sql(db, batch_id)).await.unwrap();
        for statement in split_statements(&fbpal_flush_sql(db, batch_id)) {
            fx.db.run(statement).await.unwrap();
        }
        fx.db
            .run(&console_out_flush_sql(db, batch_id))
            .await
            .unwrap();
        fx.db.run(&cpu_state_flush_sql(db, batch_id)).await.unwrap();
    }

    /// [`flush_batch`] for the batch this fixture committed last. A test
    /// here is the only writer, so that is the batch it just ran.
    async fn flush_all(fx: &Fixture) {
        flush_batch(fx, latest_batch_id(fx).await).await;
    }

    async fn latest_batch_id(fx: &Fixture) -> u64 {
        fx.db
            .fetch_one(&format!(
                "SELECT max(batch_id) FROM {}.batch_commit",
                fx.database
            ))
            .await
            .unwrap()
    }

    async fn ram_value_version(fx: &Fixture, word_addr: u32) -> (u32, u64) {
        fx.db
            .fetch_one(&format!(
                "SELECT value, version FROM {}.ram FINAL WHERE word_addr = {word_addr}",
                fx.database
            ))
            .await
            .unwrap()
    }

    async fn region_value(fx: &Fixture, table: &str, word_addr: u32) -> u32 {
        fx.db
            .fetch_one(&format!(
                "SELECT value FROM {}.{table} FINAL WHERE word_addr = {word_addr}",
                fx.database
            ))
            .await
            .unwrap()
    }

    async fn words(fx: &Fixture, table: &str, lo: u32, hi: u32) -> Vec<u32> {
        fx.db
            .fetch_one(&format!(
                "SELECT groupArray(value) FROM (\
                 SELECT value FROM {}.{table} FINAL \
                 WHERE word_addr >= {lo} AND word_addr < {hi} ORDER BY word_addr)",
                fx.database
            ))
            .await
            .unwrap()
    }

    /// The checkpoint trace's own RAM hash, taken over this test's small
    /// window. Using the trace's hash rather than a second hashing scheme
    /// keeps the comparison in the format a real differential run uses.
    fn hash_words(values: &[u32]) -> u64 {
        let bytes: Vec<u8> = values.iter().flat_map(|w| w.to_le_bytes()).collect();
        clickdoom_spec::ram_hash(&bytes)
    }

    async fn ram_hash(fx: &Fixture, ram_words: u32) -> u64 {
        hash_words(&words(fx, "ram", RAM_BASE_WORD, RAM_BASE_WORD + ram_words).await)
    }

    /// The same hash over `framebuffer` or `palette`, whose `word_addr` is
    /// relative to each region's own base and starts at 0.
    async fn region_hash(fx: &Fixture, table: &str, size_words: u32) -> u64 {
        hash_words(&words(fx, table, 0, size_words).await)
    }

    /// Blocks until every mutation on `table` has finished.
    /// `retention_sql` submits its delete asynchronously, so a row count
    /// taken straight afterwards can still see rows the delete is about to
    /// remove, and a check on that count would pass whether or not the
    /// underflow guard held.
    async fn wait_for_mutations(fx: &Fixture, table: &str) {
        let sql = format!(
            "SELECT count() FROM system.mutations \
             WHERE database = '{}' AND table = '{table}' AND is_done = 0",
            fx.database
        );
        let deadline = Instant::now() + Duration::from_secs(30); // purity-ok: harness timeout, see the import
        loop {
            let pending: u64 = fx.db.fetch_one(&sql).await.unwrap();
            if pending == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline, // purity-ok: harness timeout, see the import
                "mutations on {table} did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    async fn batch_commit_count(fx: &Fixture) -> u64 {
        fx.db
            .fetch_one(&format!("SELECT count() FROM {}.batch_commit", fx.database))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn wl_icount_absolute_across_batches() {
        let fx = Fixture::create("wl_icount_absolute_across_batches").await;
        // Two batches store to the same address. The second starts from a
        // much larger icount, but its rank within its own batch is smaller
        // than the first batch's absolute icount. A within-batch rank would
        // give both the same version, which is a tie under
        // ReplacingMergeTree and leaves the winner unspecified.
        let (decn, ram_words) = (1, 9);
        // Word `decn` is outside the text window, so no SELF_MODIFY.
        let addr = RAM_BASE + decn * 4;
        let word_addr = RAM_BASE_WORD + decn;
        seed_decoded_and_ram(&fx, &[store(1, addr, WORD)], ram_words).await;

        let mut regs = [0u32; 31];
        regs[0] = 0x1111_1111;
        fx.seed_batch_commit(0, RAM_BASE, &regs, 0).await;
        run_batch(&fx, 1, decn, ram_words).await;
        flush_all(&fx).await;
        let (v1, ver1) = ram_value_version(&fx, word_addr).await;
        assert_eq!((v1, ver1), (0x1111_1111, 1));

        regs[0] = 0x2222_2222;
        fx.seed_batch_commit(100, RAM_BASE, &regs, 60_000).await;
        run_batch(&fx, 1, decn, ram_words).await;
        flush_all(&fx).await;
        let (v2, ver2) = ram_value_version(&fx, word_addr).await;
        assert_eq!(
            ver2, 60_001,
            "the version is the starting icount plus the within-batch rank"
        );
        assert_eq!(v2, 0x2222_2222, "the chronologically later store wins");
        assert!(ver2 > ver1, "no tie between the two batches");
        fx.finish().await;
    }

    #[tokio::test]
    async fn wl_icount_three_real_chained_batches_same_address() {
        let fx = Fixture::create("wl_icount_three_chained_batches").await;
        // Two batches cannot tell a correct absolute icount from one that
        // is double-counted: doubling a rising starting icount still
        // preserves the order between batches. Three batches chained
        // through the real previous-batch lookup, all storing to the same
        // address, pin it: every intermediate version has to be present and
        // strictly increasing.
        let (decn, ram_words) = (6, 10);
        let addr = RAM_BASE + decn * 4;
        let word_addr = RAM_BASE_WORD + decn;
        let values = [0xAAAA_AAAAu32, 0xBBBB_BBBB, 0xCCCC_CCCC];
        let mut insns = Vec::new();
        for value in values {
            insns.push(addi(1, value));
            insns.push(store(1, addr, WORD));
        }
        seed_decoded_and_ram(&fx, &insns, ram_words).await;
        fx.seed_batch_commit(0, RAM_BASE, &[0u32; 31], 0).await;

        let mut versions = Vec::new();
        for value in values {
            run_batch(&fx, 2, decn, ram_words).await;
            flush_all(&fx).await;
            let (got, version) = ram_value_version(&fx, word_addr).await;
            assert_eq!(got, value, "ram FINAL must hold the write just made");
            versions.push(version);
        }
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, versions,
            "versions must be strictly increasing in write order: {versions:?}"
        );
        let (final_value, final_version) = ram_value_version(&fx, word_addr).await;
        assert_eq!(final_value, values[values.len() - 1]);
        assert_eq!(final_version, versions[versions.len() - 1]);
        fx.finish().await;
    }

    #[tokio::test]
    async fn crash_recovery_idempotent_flush() {
        let fx = Fixture::create("crash_recovery_idempotent_flush").await;
        let (decn, ram_words) = (6, 14);
        let (a, b, c) = (
            RAM_BASE + decn * 4,
            RAM_BASE + (decn + 1) * 4,
            RAM_BASE + (decn + 2) * 4,
        );
        let insns = [
            addi(1, 0x1111_1111),
            store(1, a, WORD),
            addi(2, 0x2222_2222),
            store(2, b, WORD),
            addi(3, 0x3333_3333),
            store(3, c, WORD),
        ];

        // Both sub-runs share one database, so everything the second
        // sub-run's previous-batch lookup could otherwise pick up from the
        // first has to be cleared first.
        async fn reset(fx: &Fixture, insns: &[Insn], ram_words: u32) {
            fx.truncate(&["batch_commit", "cpu_state", "console_out"])
                .await;
            seed_decoded_and_ram(fx, insns, ram_words).await;
            fx.seed_batch_commit(0, RAM_BASE, &[0u32; 31], 0).await;
        }

        reset(&fx, &insns, ram_words).await;
        for _ in 0..3 {
            run_batch(&fx, 2, decn, ram_words).await;
            flush_all(&fx).await;
        }
        let clean = ram_hash(&fx, ram_words).await;

        reset(&fx, &insns, ram_words).await;
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        // The crash window: the batch_commit row lands, the flush does not.
        run_batch(&fx, 2, decn, ram_words).await;
        // Recovery redoes the flush for the latest batch_commit row
        // unconditionally, which is what the driver does at startup.
        flush_all(&fx).await;
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        let recovered = ram_hash(&fx, ram_words).await;

        assert_eq!(
            clean, recovered,
            "a skipped then redone flush must converge to identical ram state"
        );

        // The redo has to be a no-op on an already-flushed batch too.
        let before_redo = ram_hash(&fx, ram_words).await;
        flush_all(&fx).await;
        assert_eq!(ram_hash(&fx, ram_words).await, before_redo);
        fx.finish().await;
    }

    #[derive(Row, Deserialize)]
    struct FbPalLanes {
        fb_wl_addr: Vec<u32>,
        fb_wl_val: Vec<u32>,
        fb_wl_icount: Vec<u64>,
        pal_wl_addr: Vec<u32>,
        pal_wl_val: Vec<u32>,
        pal_wl_icount: Vec<u64>,
    }

    #[tokio::test]
    async fn batch_populates_fbpal_write_log_lanes() {
        let fx = Fixture::create("batch_populates_fbpal_lanes").await;
        // That the fold computes the FRAMEBUFFER and PALETTE lanes is
        // covered by the select_only cases. This checks that `batch`'s own
        // INSERT carries them through to `batch_commit`.
        //
        // Each address is its own region's word offset, and each icount is
        // the store's absolute post-retirement icount: four instructions
        // retire in order, so the FRAMEBUFFER store is the second and the
        // PALETTE store the fourth.
        let (decn, ram_words) = (4, 8);
        let insns = [
            addi(1, 0xCAFE_BABE),
            store(1, FRAMEBUFFER_BASE, WORD),
            addi(2, 0xFEED_FACE),
            store(2, PALETTE_BASE + 4, WORD),
        ];
        seed_decoded_and_ram(&fx, &insns, ram_words).await;
        fx.seed_batch_commit(0, RAM_BASE, &[0u32; 31], 0).await;
        run_batch(&fx, 4, decn, ram_words).await;

        let row: FbPalLanes = fx
            .db
            .fetch_one(&format!(
                "SELECT fb_wl_addr, fb_wl_val, fb_wl_icount, \
                 pal_wl_addr, pal_wl_val, pal_wl_icount \
                 FROM {}.batch_commit WHERE batch_id = 1",
                fx.database
            ))
            .await
            .unwrap();
        assert_eq!(row.fb_wl_addr, vec![0]);
        assert_eq!(row.fb_wl_val, vec![0xCAFE_BABE]);
        assert_eq!(row.fb_wl_icount, vec![2]);
        assert_eq!(row.pal_wl_addr, vec![1]);
        assert_eq!(row.pal_wl_val, vec![0xFEED_FACE]);
        assert_eq!(row.pal_wl_icount, vec![4]);
        fx.finish().await;
    }

    #[tokio::test]
    async fn fbpal_crash_recovery_idempotent_flush() {
        let fx = Fixture::create("fbpal_crash_recovery_idempotent_flush").await;
        let (decn, ram_words) = (8, 12);
        let insns = [
            addi(1, 0x1111_1111),
            store(1, FRAMEBUFFER_BASE, WORD),
            addi(2, 0x2222_2222),
            store(2, FRAMEBUFFER_BASE + 4, WORD),
            addi(3, 0x3333_3333),
            store(3, PALETTE_BASE, WORD),
            addi(4, 0x4444_4444),
            store(4, PALETTE_BASE + 4, WORD),
        ];

        async fn reset(fx: &Fixture, insns: &[Insn], ram_words: u32) {
            fx.truncate(&[
                "batch_commit",
                "cpu_state",
                "console_out",
                "framebuffer",
                "palette",
            ])
            .await;
            seed_decoded_and_ram(fx, insns, ram_words).await;
            fx.seed_batch_commit(0, RAM_BASE, &[0u32; 31], 0).await;
        }

        async fn hashes(fx: &Fixture) -> (u64, u64) {
            (
                region_hash(fx, "framebuffer", 2).await,
                region_hash(fx, "palette", 2).await,
            )
        }

        reset(&fx, &insns, ram_words).await;
        for _ in 0..4 {
            run_batch(&fx, 2, decn, ram_words).await;
            flush_all(&fx).await;
        }
        let clean = hashes(&fx).await;

        // Two empty regions hash equal, so the comparison below would hold
        // even if the flush had never run. Pin down that the clean run
        // actually wrote distinguishable data first.
        assert_eq!(region_value(&fx, "framebuffer", 0).await, 0x1111_1111);
        assert_eq!(region_value(&fx, "framebuffer", 1).await, 0x2222_2222);
        assert_eq!(region_value(&fx, "palette", 0).await, 0x3333_3333);
        assert_eq!(region_value(&fx, "palette", 1).await, 0x4444_4444);

        reset(&fx, &insns, ram_words).await;
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        // The crash window: the batch_commit row lands, the flush does not.
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        run_batch(&fx, 2, decn, ram_words).await;
        flush_all(&fx).await;
        let recovered = hashes(&fx).await;

        assert_eq!(
            clean, recovered,
            "a skipped then redone fbpal flush must converge to identical state"
        );

        let before_redo = hashes(&fx).await;
        flush_all(&fx).await;
        assert_eq!(hashes(&fx).await, before_redo);
        fx.finish().await;
    }

    #[tokio::test]
    async fn a_flush_derives_the_batch_it_names_not_the_latest() {
        let fx = Fixture::create("flush_derives_the_batch_it_names").await;
        // Two runners against one database: A commits batch 1, B commits
        // batch 2, and A then flushes. A flush that read `max(batch_id)`
        // would derive B's batch and drop A's write-log with no error
        // anywhere, so batch 1's store would never reach `ram`.
        let (decn, ram_words) = (1, 9);
        let addr = RAM_BASE + decn * 4;
        let word_addr = RAM_BASE_WORD + decn;
        seed_decoded_and_ram(&fx, &[store(1, addr, WORD)], ram_words).await;

        let mut regs = [0u32; 31];
        regs[0] = 0xAAAA_AAAA;
        fx.seed_batch_commit(0, RAM_BASE, &regs, 0).await;
        run_batch(&fx, 1, decn, ram_words).await;
        // The second batch chains off the first, so it stores the same
        // value at the same address. Its write-log entry carries icount 2
        // where the first carries 1, which is what tells the two apart in
        // `ram.version`.
        run_batch(&fx, 1, decn, ram_words).await;
        assert_eq!(latest_batch_id(&fx).await, 2, "two batches are committed");

        flush_batch(&fx, 1).await;
        let (value, version) = ram_value_version(&fx, word_addr).await;
        assert_eq!(
            (value, version),
            (0xAAAA_AAAA, 1),
            "the flush derived a batch other than the one it was given"
        );
        let flushed: u64 = fx
            .db
            .fetch_one(&format!(
                "SELECT count() FROM {}.cpu_state FINAL",
                fx.database
            ))
            .await
            .unwrap();
        assert_eq!(flushed, 1, "only the named batch reached cpu_state");
        fx.finish().await;
    }

    #[tokio::test]
    async fn retention_delete_does_not_underflow_early_in_a_run() {
        let fx = Fixture::create("retention_does_not_underflow").await;
        // On the first batches of a run, `max(batch_id) - N` computed in
        // unsigned space wraps to a huge value, and `batch_id < <huge>`
        // matches every row, including the one just committed.
        fx.seed_batch_commit(0, RAM_BASE, &[0u32; 31], 0).await;
        let before = batch_commit_count(&fx).await;
        assert_eq!(before, 1);
        fx.db
            .run(&retention_sql(&fx.database, 0, BATCH_COMMIT_RETENTION_N))
            .await
            .unwrap();
        wait_for_mutations(&fx, "batch_commit").await;
        assert_eq!(
            batch_commit_count(&fx).await,
            before,
            "retention deleted a row inside its own lag window"
        );
        fx.finish().await;
    }
}
