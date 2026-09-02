//! The ClickHouse connection every subcommand shares.
//!
//! One persistent connection per process, reused for every statement. A
//! statement's result is always propagated, never discarded: a failed query
//! is a typed error carrying the server's own message, not a silent no-op.

use std::env;

use clap::Args;
use clickhouse::{Client, Compression};

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
    /// The password to authenticate with: the flag, else
    /// `$CLICKHOUSE_PASSWORD`, else empty.
    pub fn resolved_password(&self) -> String {
        self.password
            .clone()
            .or_else(|| env::var("CLICKHOUSE_PASSWORD").ok())
            .unwrap_or_default()
    }

    /// Opens a client. Opening does not itself connect; the first statement
    /// does.
    pub fn connect(&self) -> Db {
        Db {
            client: self.client(),
        }
    }

    /// [`connect`](ConnArgs::connect) with compression off in both
    /// directions, for statements whose payload is a few bytes and whose
    /// cost is the round trip.
    pub fn connect_uncompressed(&self) -> Db {
        Db {
            client: self
                .client()
                .with_compression(Compression::None)
                .with_setting("enable_http_compression", "0"),
        }
    }

    fn client(&self) -> Client {
        Client::default()
            .with_url(format!("http://{}:{}", self.host, self.port))
            .with_user(&self.user)
            .with_password(self.resolved_password())
            .with_database(&self.database)
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

    /// Runs a statement that returns no rows under `query_id`, so
    /// `system.query_log` can be read back for this one statement.
    /// ClickHouse's HTTP handler reserves `query_id` as a URL parameter and
    /// takes it as the query's identity rather than passing it on as a
    /// setting.
    pub async fn run_with_query_id(&self, query_id: &str, sql: &str) -> Result<(), Error> {
        self.client
            .query(sql)
            .with_setting("query_id", query_id)
            .execute()
            .await?;
        Ok(())
    }

    /// Runs an `INSERT ... FORMAT <format>` whose rows travel as the request
    /// body rather than inside the statement text.
    ///
    /// The body reaches the server as it stands. Nothing here parses it,
    /// reorders it or fills anything in; the statement's own column list and
    /// format say how the server reads it.
    pub async fn run_with_body(&self, sql: &str, body: bytes::Bytes) -> Result<(), Error> {
        let mut insert = self.client.insert_formatted_with(sql);
        insert.send(body).await?;
        insert.end().await?;
        Ok(())
    }

    /// Runs a statement and fetches a single row.
    pub async fn fetch_one<T>(&self, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        Ok(self.client.query(sql).fetch_one::<T>().await?)
    }

    /// Runs a statement and fetches a single row under `query_id`. Same
    /// identity rule as [`Db::run_with_query_id`].
    pub async fn fetch_one_with_query_id<T>(&self, query_id: &str, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        Ok(self
            .client
            .query(sql)
            .with_setting("query_id", query_id)
            .fetch_one::<T>()
            .await?)
    }

    /// Runs a statement and fetches every row.
    pub async fn fetch_all<T>(&self, sql: &str) -> Result<Vec<T>, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        let mut cursor = self.client.query(sql).fetch::<T>()?;
        let mut rows = Vec::new();
        while let Some(row) = cursor.next().await? {
            rows.push(row);
        }
        Ok(rows)
    }

    /// Runs a statement that returns no rows, with `param_<name>` query
    /// parameters bound for a `{name:Type}` placeholder in the SQL text.
    pub async fn run_with_params(&self, sql: &str, params: &[(&str, u32)]) -> Result<(), Error> {
        let mut query = self.client.query(sql);
        for (name, value) in params {
            query = query.param(name, *value);
        }
        query.execute().await?;
        Ok(())
    }

    /// Inserts every row of `rows` into `table`, naming the columns after
    /// `T`'s own fields (a `#[derive(Row)]` type inserts into exactly the
    /// columns it declares, leaving the rest at their table default).
    pub async fn insert_all<T>(
        &self,
        table: &str,
        rows: impl Iterator<Item = T>,
    ) -> Result<(), Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowWrite,
    {
        let mut insert = self.client.insert::<T>(table).await?;
        for row in rows {
            insert.write(&row).await?;
        }
        insert.end().await?;
        Ok(())
    }
}
