//! A private database per test, carrying the real `sqlcpu/schema.sql`.
//!
//! The schema is the file `sqlcpu` ships, renamed onto a database named
//! after this process and the calling test, so no column shape can drift
//! from what the engine actually runs against and nothing here touches the
//! shared `clickdoom` database. One database per test rather than one per
//! run: tests inside a binary run in parallel threads, and a shared
//! `decoded`/`ram`/`batch_commit` would let them overwrite each other.

use clickhouse::Row;
use serde::Serialize;

use super::RAM_BASE_WORD;
use super::db::{Conn, Db};
use super::reference::Insn;

const SCHEMA: &str = include_str!("../../../sqlcpu/schema.sql");

/// One `decoded` row, carrying every column the table declares without a
/// default. The column names are `sqlcpu`'s own (`id`, `tgt`, `mk`, `sg`).
/// The decode-time flags from `m_sg1` to `tgt_mis` select collapsed
/// execute arms the fold does not read, so a hand-built row leaves them
/// at 0.
#[derive(Row, Serialize)]
pub struct DecodedRow {
    pub word_addr: u32,
    pub id: u8,
    pub rd: u8,
    pub rs1: u8,
    pub rs2: u8,
    pub imm: u32,
    pub tgt: u32,
    pub mk: u32,
    pub sg: u8,
    pub m_sg1: u8,
    pub m_sg2: u8,
    pub m_hi: u8,
    pub d_sg: u8,
    pub cmp_sel: u8,
    pub neg: u8,
    pub tgt_mis: u8,
    pub raw: u32,
}

#[derive(Row, Serialize)]
pub struct RamRow {
    pub word_addr: u32,
    pub value: u32,
    pub version: u64,
}

#[derive(Row, Serialize)]
pub struct InputQueueRow {
    pub event_seq: u64,
    pub key_event: u16,
    pub consumed: u8,
}

/// One `batch_commit` row as a test seeds it: a previous batch's state,
/// with every write-log lane empty.
#[derive(Row, Serialize)]
pub struct BatchCommitSeed {
    pub batch_id: u64,
    pub icount: u64,
    pub pc: u32,
    pub regs: Vec<u32>,
    pub halted: u8,
    pub halt_reason: String,
    pub exit_code: u32,
    pub keyq_pos: u64,
    pub has_frame: u8,
    pub frame_no: u32,
    pub wl_addr: Vec<u32>,
    pub wl_val: Vec<u32>,
    pub wl_icount: Vec<u64>,
    pub fb_wl_addr: Vec<u32>,
    pub fb_wl_val: Vec<u32>,
    pub fb_wl_icount: Vec<u64>,
    pub pal_wl_addr: Vec<u32>,
    pub pal_wl_val: Vec<u32>,
    pub pal_wl_icount: Vec<u64>,
    pub console_bytes: Vec<u8>,
}

pub struct Fixture {
    conn: Conn,
    pub database: String,
    pub db: Db,
}

impl Fixture {
    /// Creates the private database for `case` and applies the schema.
    pub async fn create(case: &str) -> Fixture {
        let conn = Conn::from_env();
        let database = format!("clickdoom_executor_test_{}_{case}", std::process::id());
        let admin = conn.open("default");
        admin
            .run(&format!("DROP DATABASE IF EXISTS {database}"))
            .await
            .unwrap();
        let renamed = SCHEMA
            .replace("clickdoom.", &format!("{database}."))
            .replace(
                "CREATE DATABASE IF NOT EXISTS clickdoom;",
                &format!("CREATE DATABASE IF NOT EXISTS {database};"),
            );
        // If the schema ever stopped spelling its database the way this
        // rename expects, every statement below would land in the shared
        // `clickdoom` database instead, and nothing would say so.
        assert!(
            !renamed.contains("clickdoom.")
                && renamed.contains(&format!("CREATE DATABASE IF NOT EXISTS {database};")),
            "the schema still names the shared database after renaming"
        );
        admin.run_script(&renamed).await.unwrap();
        let db = conn.open(&database);
        Fixture { conn, database, db }
    }

    /// Drops the private database.
    pub async fn finish(self) {
        let admin = self.conn.open("default");
        admin
            .run(&format!("DROP DATABASE IF EXISTS {}", self.database))
            .await
            .unwrap();
    }

    pub async fn truncate(&self, tables: &[&str]) {
        for table in tables {
            self.db
                .run(&format!("TRUNCATE TABLE {}.{table}", self.database))
                .await
                .unwrap();
        }
    }

    /// Writes `insns` into `decoded` at consecutive word addresses starting
    /// at RAM_BASE.
    pub async fn seed_decoded(&self, insns: &[Insn]) {
        let rows = insns.iter().enumerate().map(|(i, ins)| DecodedRow {
            word_addr: RAM_BASE_WORD + i as u32,
            id: ins.op_id as u8,
            rd: ins.rd,
            rs1: ins.rs1,
            rs2: ins.rs2,
            imm: ins.imm,
            tgt: ins.target,
            mk: ins.width_mask,
            sg: ins.sign_bit,
            m_sg1: 0,
            m_sg2: 0,
            m_hi: 0,
            d_sg: 0,
            cmp_sel: 0,
            neg: 0,
            tgt_mis: 0,
            raw: ins.raw,
        });
        self.db.insert_all("decoded", rows).await.unwrap();
    }

    /// Writes `words` into `ram` densely from RAM_BASE. The fold captures
    /// `ram` as one positionally indexed array, so every word in the window
    /// needs a row, including the zero ones.
    pub async fn seed_ram(&self, words: &[u32]) {
        let rows = words.iter().enumerate().map(|(i, value)| RamRow {
            word_addr: RAM_BASE_WORD + i as u32,
            value: *value,
            version: 0,
        });
        self.db.insert_all("ram", rows).await.unwrap();
    }

    /// Writes `events` into `input_queue` in `event_seq` order, the shape
    /// the fold's `KEYQT` capture reads.
    pub async fn seed_input_queue(&self, events: &[u16]) {
        if events.is_empty() {
            return;
        }
        let rows = events
            .iter()
            .enumerate()
            .map(|(seq, key_event)| InputQueueRow {
                event_seq: seq as u64,
                key_event: *key_event,
                consumed: 0,
            });
        self.db.insert_all("input_queue", rows).await.unwrap();
    }

    /// Seeds one `batch_commit` row as the next batch's previous state.
    /// Written directly rather than through the driver's bootstrap, so a
    /// test can choose `pc`, `regs` and `icount` precisely.
    pub async fn seed_batch_commit(&self, batch_id: u64, pc: u32, regs: &[u32], icount: u64) {
        assert_eq!(regs.len(), 31, "regs is x1..x31");
        let row = BatchCommitSeed {
            batch_id,
            icount,
            pc,
            regs: regs.to_vec(),
            halted: 0,
            halt_reason: String::new(),
            exit_code: 0,
            keyq_pos: 0,
            has_frame: 0,
            frame_no: 0,
            wl_addr: Vec::new(),
            wl_val: Vec::new(),
            wl_icount: Vec::new(),
            fb_wl_addr: Vec::new(),
            fb_wl_val: Vec::new(),
            fb_wl_icount: Vec::new(),
            pal_wl_addr: Vec::new(),
            pal_wl_val: Vec::new(),
            pal_wl_icount: Vec::new(),
            console_bytes: Vec::new(),
        };
        self.db
            .insert_all("batch_commit", std::iter::once(row))
            .await
            .unwrap();
    }
}
