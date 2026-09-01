//! Which compilation regime a statement ran in, read back from
//! `system.query_log`.
//!
//! ClickHouse compiles an expression DAG once it has executed
//! `min_count_to_compile_expression` times, and counts those executions in a
//! process-static map that no `SYSTEM` statement resets. A statement that
//! pays for the compilation measures different work than the one after it,
//! so every batch this benchmark reports carries the two profile events that
//! say whether it paid.

use std::collections::HashMap;

use crate::client::{Db, Error};

/// The compilation a single statement did.
///
/// `CompileFunction` counts LLVM compilations the statement started, so it
/// is non-zero only on a cache miss past the threshold. Zero means the
/// statement either found the compiled function in the cache or never
/// crossed the threshold; the two are told apart by looking at the whole
/// sequence of statements on the same server, not at one row.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Regime {
    /// `ProfileEvents['CompileFunction']`.
    pub compile_function: u64,
    /// `ProfileEvents['CompileExpressionsMicroseconds']`.
    pub compile_micros: u64,
    /// `query_duration_ms`, the server's own time on the statement. Client
    /// wall clock also carries the result set's serialisation, transfer and
    /// deserialisation, and the two arms return result sets of very
    /// different sizes.
    pub server_ms: u64,
}

impl Regime {
    /// Folds another statement's events in, for a timed region built from
    /// more than one statement.
    pub fn add(&mut self, other: Regime) {
        self.compile_function += other.compile_function;
        self.compile_micros += other.compile_micros;
        self.server_ms += other.server_ms;
    }
}

/// A `query_id` this benchmark issues a statement under. Every character is
/// from `[A-Za-z0-9_]`, so the id needs no quoting in the `system.query_log`
/// lookup and cannot carry SQL of its own.
pub fn query_id(run: u32, window: &str, mode: &str, batch: u32, statement: usize) -> String {
    let window: String = window
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("clickdoom_bench_{run}_{window}_{mode}_{batch}_{statement}")
}

/// Reads the compilation events and the server-side duration of every named
/// statement.
///
/// Flushes `query_log` first: a finished statement is not in the table until
/// its buffer is written out, and a missing row would otherwise read as
/// "compiled nothing".
pub async fn read(db: &Db, query_ids: &[String]) -> Result<HashMap<String, Regime>, Error> {
    if query_ids.is_empty() {
        return Ok(HashMap::new());
    }
    db.run("SYSTEM FLUSH LOGS").await?;
    let list = query_ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    let rows: Vec<(String, u64, u64, u64)> = db
        .fetch_all(&format!(
            "SELECT query_id,\n       \
             toUInt64(ProfileEvents['CompileFunction']),\n       \
             toUInt64(ProfileEvents['CompileExpressionsMicroseconds']),\n       \
             toUInt64(query_duration_ms)\n\
             FROM system.query_log\n\
             WHERE type = 'QueryFinish' AND query_id IN ({list})"
        ))
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, compile_function, compile_micros, server_ms)| {
            (
                id,
                Regime {
                    compile_function,
                    compile_micros,
                    server_ms,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_id_carries_no_sql() {
        let id = query_id(7, "boot: from icount 0, to frame 0", "fold", 3, 0);
        assert_eq!(
            id,
            "clickdoom_bench_7_boot__from_icount_0__to_frame_0_fold_3_0"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "{id}"
        );
    }

    #[test]
    fn ids_of_different_batches_differ() {
        let a = query_id(1, "boot", "fold", 1, 0);
        let b = query_id(1, "boot", "fold", 2, 0);
        let c = query_id(1, "boot", "e2e", 1, 0);
        let d = query_id(1, "boot", "fold", 1, 1);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn adding_sums_both_events() {
        let mut total = Regime::default();
        total.add(Regime {
            compile_function: 2,
            compile_micros: 100,
            server_ms: 900,
        });
        total.add(Regime {
            compile_function: 1,
            compile_micros: 50,
            server_ms: 30,
        });
        assert_eq!(total.compile_function, 3);
        assert_eq!(total.compile_micros, 150);
        assert_eq!(total.server_ms, 930);
    }
}
