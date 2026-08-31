//! The ClickHouse connection the live tests run their SQL through.
//!
//! One client per test, reused for every statement that test issues. A
//! statement's result is always propagated: a failed query is a typed error
//! carrying the server's own message, never a silent no-op.

use std::env;

use clickhouse::Client;

/// Where the server is. Read from `CLICKHOUSE_HOST`,
/// `CLICKHOUSE_HTTP_PORT` and `CLICKHOUSE_PASSWORD`, defaulting to
/// `localhost:8123` with no password.
#[derive(Clone, Debug)]
pub struct Conn {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl Conn {
    pub fn from_env() -> Conn {
        Conn {
            host: env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
            port: env::var("CLICKHOUSE_HTTP_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8123),
            user: env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_owned()),
            password: env::var("CLICKHOUSE_PASSWORD").unwrap_or_default(),
        }
    }

    /// Opens a client against `database`. Opening does not connect; the
    /// first statement does.
    pub fn open(&self, database: &str) -> Db {
        self.open_with(database, &[])
    }

    /// [`open`](Conn::open) with `settings` on every statement the client
    /// issues, for asking the server for something a query's own
    /// `SETTINGS` clause has to override.
    pub fn open_with(&self, database: &str, settings: &[(&str, &str)]) -> Db {
        let mut client = Client::default()
            .with_url(format!("http://{}:{}", self.host, self.port))
            .with_user(&self.user)
            .with_password(&self.password)
            // The parser stops at this many bytes before it reaches the
            // SETTINGS clause that raises it, so the value the generated
            // fold SQL asks for has to be set on the connection too.
            .with_setting("max_query_size", "2000000")
            .with_database(database);
        for (name, value) in settings {
            client = client.with_setting(*name, *value);
        }
        Db { client }
    }
}

/// A failed statement, carrying the server's own message and enough of the
/// SQL to identify it. Only the head is kept: a generated fold query runs
/// to tens of kilobytes.
#[derive(Debug, thiserror::Error)]
#[error("statement failed: {head}")]
pub struct Error {
    pub head: String,
    #[source]
    pub source: clickhouse::error::Error,
}

fn head(sql: &str) -> String {
    let line = sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    match line.char_indices().nth(120) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// One ClickHouse connection, reused for every statement issued through it.
pub struct Db {
    client: Client,
}

impl Db {
    /// Runs a statement that returns no rows: DDL, a `TRUNCATE`, an
    /// `INSERT`, a flush.
    pub async fn run(&self, sql: &str) -> Result<(), Error> {
        self.client
            .query(sql)
            .execute()
            .await
            .map_err(|source| Error {
                head: head(sql),
                source,
            })
    }

    /// Runs every statement in a multi-statement script, in order. The HTTP
    /// interface takes one statement per request.
    pub async fn run_script(&self, script: &str) -> Result<(), Error> {
        for statement in split_statements(script) {
            self.run(statement).await?;
        }
        Ok(())
    }

    /// Runs a statement and fetches a single row.
    pub async fn fetch_one<T>(&self, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        self.client
            .query(sql)
            .fetch_one::<T>()
            .await
            .map_err(|source| Error {
                head: head(sql),
                source,
            })
    }

    /// Inserts every row of `rows` into `table`, naming the columns after
    /// `T`'s own fields. Every column the table declares without a
    /// `DEFAULT` has to be one of them, or the server rejects the insert.
    pub async fn insert_all<T>(
        &self,
        table: &str,
        rows: impl Iterator<Item = T>,
    ) -> Result<(), Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowWrite,
    {
        let fail = |source| Error {
            head: format!("INSERT INTO {table}"),
            source,
        };
        let mut insert = self.client.insert::<T>(table).await.map_err(fail)?;
        for row in rows {
            insert.write(&row).await.map_err(fail)?;
        }
        insert.end().await.map_err(fail)
    }
}

/// Splits `text` on the `;` characters outside a `--` line comment and
/// outside a single-quoted string. Drops empty pieces.
///
/// `driver/src/sql.rs` does the same split for the driver and carries the
/// tests for it. `clickdoom-executor` does not depend on the driver, so the
/// tests here carry their own.
pub fn split_statements(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_comment {
            if b == b'\n' {
                in_comment = false;
            }
        } else if in_string {
            if b == b'\'' {
                in_string = false;
            }
        } else {
            match b {
                b'\'' => in_string = true,
                b'-' if bytes.get(i + 1) == Some(&b'-') => in_comment = true,
                b';' => {
                    let piece = text[start..i].trim();
                    if !piece.is_empty() {
                        statements.push(piece);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        statements.push(tail);
    }
    statements
}
