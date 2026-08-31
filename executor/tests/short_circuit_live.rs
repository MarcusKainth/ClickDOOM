//! The `short_circuit_function_evaluation` setting the fold pins, checked
//! against a running server.
//!
//! At `'disable'` ClickHouse evaluates every argument of an `if` or a
//! `multiIf` on every row, so an arm runs even on the rows its guard
//! rejects. The fold pins `'disable'` in its own `SETTINGS` clause. These
//! cases require that a query's own clause is what the server applies, and
//! that the fold's divide and remainder arms survive the rule on the input
//! that reaches a zero divisor.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.

#[cfg(feature = "clickhouse-tests")]
mod support;

#[cfg(feature = "clickhouse-tests")]
mod live {
    use super::support::db::Conn;
    use super::support::fixture::Fixture;
    use super::support::fold_case::{FoldCase, run_checked_through};
    use super::support::insn::{addi, alu};

    const SETTING: &str = "short_circuit_function_evaluation";

    /// Divides by the value its guard rejects. At `'enable'` the else arm
    /// is skipped and the query returns 4294967295. At `'disable'` the arm
    /// runs and the server raises Code 153 ILLEGAL_DIVISION.
    const CANARY: &str = "SELECT if(number = 0, toUInt64(4294967295), intDiv(toUInt64(1), number)) AS v FROM numbers(1)";

    /// The `id` values of DIV, DIVU, REM and REMU, the four arms that
    /// divide. `support/reference.rs` answers for the same four.
    const DIVIDE_ARMS: [u32; 4] = [14, 15, 16, 17];

    /// Requires `sql` to fail with a division by zero, rather than with
    /// whatever error a connection or a typo would also produce.
    async fn assert_illegal_division(db: &super::support::db::Db, sql: &str, case: &str) {
        let err = db
            .fetch_one::<u64>(sql)
            .await
            .err()
            .unwrap_or_else(|| panic!("{case}: the query returned instead of failing"));
        let text = format!("{}", std::error::Error::source(&err).unwrap());
        assert!(
            text.contains("ILLEGAL_DIVISION"),
            "{case}: failed with something other than a division by zero: {text}"
        );
    }

    /// What the server applied to this fixture's fold query, read back from
    /// `system.query_log` rather than taken from the query text.
    async fn applied_setting(fx: &Fixture) -> String {
        let admin = Conn::from_env().open("default");
        admin.run("SYSTEM FLUSH LOGS").await.unwrap();
        admin
            .fetch_one::<String>(&format!(
                "SELECT CAST(Settings['{SETTING}'], 'String')\n\
                 FROM system.query_log\n\
                 WHERE type = 'QueryFinish' AND query LIKE '%arrayFold%'\n  \
                 AND query LIKE '%{}.decoded%'\n\
                 ORDER BY event_time_microseconds DESC LIMIT 1",
                fx.database
            ))
            .await
            .unwrap()
    }

    /// Without this the readback below proves nothing: a server whose
    /// profile already says `'disable'` would report `'disable'` for a fold
    /// query carrying no clause of its own.
    #[tokio::test]
    async fn a_settings_clause_on_the_query_beats_the_session() {
        let conn = Conn::from_env();
        let asks_enable = conn.open_with("default", &[(SETTING, "enable")]);
        let asks_disable = conn.open_with("default", &[(SETTING, "disable")]);

        // With no clause of its own, a query gets what its session asked
        // for, which is what makes the canary a discriminator at all.
        assert_eq!(
            asks_enable.fetch_one::<u64>(CANARY).await.unwrap(),
            4_294_967_295,
            "the canary has to survive at 'enable'"
        );
        assert_illegal_division(&asks_disable, CANARY, "the session asked for 'disable'").await;

        // A clause of its own wins in both directions.
        assert_illegal_division(
            &asks_enable,
            &format!("{CANARY} SETTINGS {SETTING} = 'disable'"),
            "a pinned 'disable' against a session asking for 'enable'",
        )
        .await;
        assert_eq!(
            asks_disable
                .fetch_one::<u64>(&format!("{CANARY} SETTINGS {SETTING} = 'enable'"))
                .await
                .unwrap(),
            4_294_967_295,
            "a pinned 'enable' has to beat a session asking for 'disable'"
        );
    }

    /// `rs2 = x0` makes the second operand 0, which is the input that
    /// reaches `intDiv`'s and `modulo`'s zero divisor once nothing
    /// short-circuits. Each arm writes its own register, so a query
    /// answering all four the same way still fails.
    #[tokio::test]
    async fn the_fold_runs_at_disable_when_the_session_asks_for_enable() {
        let fx = Fixture::create("short_circuit_pin").await;
        let asks_enable = fx.db_with_settings(&[(SETTING, "enable")]);

        let mut insns = vec![addi(1, 7)];
        insns.extend(
            DIVIDE_ARMS
                .iter()
                .enumerate()
                .map(|(i, id)| alu(*id, 2 + i as u8, 1, 0)),
        );
        let row = run_checked_through(
            &asks_enable,
            &fx,
            &FoldCase {
                insns: &insns,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(row.x(2), 0xFFFF_FFFF, "div by zero is all ones");
        assert_eq!(row.x(3), 0xFFFF_FFFF, "divu by zero is all ones");
        assert_eq!(row.x(4), 7, "rem by zero is the dividend");
        assert_eq!(row.x(5), 7, "remu by zero is the dividend");

        // An unpinned fold query records no value here, because a session
        // asking for the profile's own value is not a change. Comparing
        // against 'disable' catches that reading and an explicit pin of
        // the wrong value alike.
        assert_eq!(
            applied_setting(&fx).await,
            "disable",
            "the fold's own SETTINGS clause is not pinning {SETTING}"
        );
        fx.finish().await;
    }
}
