//! The ClickHouse connection every subcommand shares.
//!
//! One persistent connection per process, reused for every statement. A
//! statement's result is always propagated, never discarded: a failed query
//! is a typed error carrying the server's own message, not a silent no-op.

use std::env;

use clap::Args;
use clickhouse::Client;

/// Where the database is and how to reach it, shared by every subcommand.
///
/// The password never appears on the command line unless the caller puts it
/// there: it defaults to `$CLICKHOUSE_PASSWORD`, read once in-process, so a
/// hung or crashed `clickdoom` never shows its password in `ps`.
#[derive(Args, Clone, Debug)]
pub struct ConnArgs {
    /// ClickHouse HTTP host
    #[arg(long, default_value = "localhost")]
    pub host: String,
    /// ClickHouse HTTP port
    #[arg(long, default_value_t = 8123)]
    pub port: u16,
    /// ClickHouse user
    #[arg(long, default_value = "default")]
    pub user: String,
    /// Database to connect to
    #[arg(long, default_value = "clickdoom")]
    pub database: String,
    /// ClickHouse password. Defaults to $CLICKHOUSE_PASSWORD, then empty
    #[arg(long)]
    pub password: Option<String>,
}

impl ConnArgs {
    fn resolved_password(&self) -> String {
        self.password
            .clone()
            .or_else(|| env::var("CLICKHOUSE_PASSWORD").ok())
            .unwrap_or_default()
    }

    /// Opens a client. Opening does not itself connect; the first statement
    /// does.
    pub fn connect(&self) -> Db {
        let url = format!("http://{}:{}", self.host, self.port);
        let client = Client::default()
            .with_url(url)
            .with_user(&self.user)
            .with_password(self.resolved_password())
            .with_database(&self.database);
        Db { client }
    }
}

/// A ClickHouse error, carrying the server's own message.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error(#[from] clickhouse::error::Error);

/// One ClickHouse connection, reused for every statement issued through it.
pub struct Db {
    client: Client,
}

impl Db {
    /// Runs a statement that returns no rows: DDL, an `INSERT`, an
    /// `arrayFold` batch executed for its side effect.
    pub async fn run(&self, sql: &str) -> Result<(), Error> {
        self.client.query(sql).execute().await?;
        Ok(())
    }

    /// Runs a statement and fetches a single row.
    pub async fn fetch_one<T>(&self, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        Ok(self.client.query(sql).fetch_one::<T>().await?)
    }
}
