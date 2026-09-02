//! Running a plan against a real ClickHouse, for the live tests.
//!
//! One private database per test, named for this process and the test, so
//! nothing here touches the shared `clickdoom` database and two tests in
//! one binary cannot overwrite each other.
//!
//! This is the whole of what executes a plan: issue each statement in
//! order, send the body when there is one, propagate the server's own
//! error. It decides nothing.

use std::env;

use clickhouse::Client;

use clickdoom_native::sql::Statement;

/// A failed statement, carrying the server's message and enough of the SQL
/// to identify it.
#[derive(Debug, thiserror::Error)]
#[error("statement failed: {head}")]
pub struct Error {
    pub head: String,
    #[source]
    pub source: clickhouse::error::Error,
}

fn head(sql: &str) -> String {
    let line = sql.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// Where the server is. Read from `CLICKHOUSE_HOST`,
/// `CLICKHOUSE_HTTP_PORT` and `CLICKHOUSE_PASSWORD`, defaulting to
/// `localhost:8123` with no password.
pub fn client(database: &str) -> Client {
    let host = env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned());
    let port = env::var("CLICKHOUSE_HTTP_PORT").unwrap_or_else(|_| "8123".to_owned());
    Client::default()
        .with_url(format!("http://{host}:{port}"))
        .with_user(env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_owned()))
        .with_password(env::var("CLICKHOUSE_PASSWORD").unwrap_or_default())
        .with_database(database)
}

/// A private database, dropped when the test ends.
pub struct Fixture {
    pub database: String,
    pub db: Client,
}

impl Fixture {
    /// The connection stays on `default`, because the plan's first
    /// statement is what creates this database. Every statement the plan
    /// issues names its database, so nothing depends on the connection's.
    pub async fn create(case: &str) -> Fixture {
        let database = format!("clickdoom_native_test_{}_{case}", std::process::id());
        let db = client("default");
        run(
            &db,
            &Statement::sql(format!("DROP DATABASE IF EXISTS {database}")),
        )
        .await
        .unwrap();
        Fixture { db, database }
    }

    /// Issues every statement in order. The first failure stops the run
    /// and is returned with the statement that caused it.
    pub async fn execute(&self, plan: &[Statement]) -> Result<(), Error> {
        for statement in plan {
            run(&self.db, statement).await?;
        }
        Ok(())
    }

    pub async fn count(&self, table: &str) -> u64 {
        self.scalar(&format!("SELECT count() FROM {}.{table}", self.database))
            .await
    }

    pub async fn scalar<T>(&self, sql: &str) -> T
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        self.db
            .query(sql)
            .fetch_one::<T>()
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", head(sql)))
    }

    pub async fn rows<T>(&self, sql: &str) -> Vec<T>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        self.db
            .query(sql)
            .fetch_all::<T>()
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", head(sql)))
    }

    pub async fn finish(self) {
        run(
            &self.db,
            &Statement::sql(format!("DROP DATABASE IF EXISTS {}", self.database)),
        )
        .await
        .unwrap();
    }
}

/// One statement. A body goes as the request body; the HTTP interface
/// takes one statement per request either way.
pub async fn run(db: &Client, statement: &Statement) -> Result<(), Error> {
    let fail = |source| Error {
        head: head(&statement.sql),
        source,
    };
    if statement.body.is_empty() {
        return db.query(&statement.sql).execute().await.map_err(fail);
    }
    let mut insert = db.insert_formatted_with(&statement.sql);
    insert
        .send(bytes::Bytes::from(statement.body.clone()))
        .await
        .map_err(fail)?;
    insert.end().await.map_err(fail)
}
