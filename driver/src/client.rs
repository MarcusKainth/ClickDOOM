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

impl Error {
    /// Whether the connection this request went out on was closed under it.
    ///
    /// The client holds a pool of connections and the server closes one of
    /// its own accord once its keep-alive limits are reached, so a request
    /// can be handed to a connection that is already gone. A connection
    /// that could not be opened at all is not one of these: there the
    /// server is unreachable rather than the connection stale.
    pub fn on_a_closed_connection(&self) -> bool {
        let clickhouse::error::Error::Network(source) = &self.0 else {
            return false;
        };
        source
            .downcast_ref::<hyper_util::client::legacy::Error>()
            .is_some_and(|network| !network.is_connect())
    }
}

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
    ///
    /// The rest of the response is read and dropped, so the row is the
    /// first one whatever the statement returns.
    pub async fn fetch_one<T>(&self, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        first_row(self.client.query(sql)).await
    }

    /// Runs a read and fetches a single row, going out once more on a
    /// fresh connection when the connection the first attempt used was
    /// closed under it.
    ///
    /// One further attempt, for that reason alone: every other failure is
    /// reported as it stands. `sql` has to be a read, because it can run
    /// twice.
    pub async fn fetch_one_reconnecting<T>(&self, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        match first_row(self.client.query(sql)).await {
            // The failed attempt takes the dead connection out of the
            // pool, so this one goes out on a connection of its own.
            Err(error) if error.on_a_closed_connection() => first_row(self.client.query(sql)).await,
            outcome => outcome,
        }
    }

    /// Runs a statement and fetches a single row under `query_id`. Same
    /// identity rule as [`Db::run_with_query_id`].
    pub async fn fetch_one_with_query_id<T>(&self, query_id: &str, sql: &str) -> Result<T, Error>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        first_row(self.client.query(sql).with_setting("query_id", query_id)).await
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

/// The first row of `query`, with the rest of the response read and
/// dropped.
///
/// Reading to the end is what returns the connection to the client's pool.
/// A read that stops at the row leaves the response unfinished, so the
/// connection is dropped and the next read opens another one.
async fn first_row<T>(query: clickhouse::query::Query) -> Result<T, Error>
where
    T: clickhouse::RowOwned + clickhouse::RowRead,
{
    let mut cursor = query.fetch::<T>()?;
    let first = cursor
        .next()
        .await?
        .ok_or(clickhouse::error::Error::RowNotFound)?;
    while cursor.next().await?.is_some() {}
    Ok(first)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration; // purity-ok: a bound on what a test waits for, never a value a statement reads

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// One `UInt32` column named `v`, in the format the client's cursor
    /// reads: the column count, the name, the type, then the value.
    fn one_row(value: u32) -> Vec<u8> {
        let mut body = vec![1, 1, b'v', 6];
        body.extend_from_slice(b"UInt32");
        body.extend_from_slice(&value.to_le_bytes());
        body
    }

    fn response(status: &str, body: &[u8], headers: &str) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\n\
             Content-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// A server that answers each connection the way `answers` says, in
    /// order, and hangs up where the answer is `None`. Reports how many
    /// connections it took.
    fn serve(answers: Vec<Option<Vec<u8>>>) -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a local port");
        let port = listener.local_addr().expect("the bound address").port();
        listener.set_nonblocking(true).expect("a pollable listener");
        let taken = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&taken);
        tokio::spawn(async move {
            let listener = TcpListener::from_std(listener).expect("a tokio listener");
            for answer in answers {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counted.fetch_add(1, Ordering::SeqCst);
                let mut seen = [0u8; 4096];
                let _ = socket.read(&mut seen).await;
                if let Some(answer) = answer {
                    let _ = socket.write_all(&answer).await;
                    let _ = socket.flush().await;
                }
                // Dropping the socket is the server closing the
                // connection, answered or not.
            }
        });
        (port, taken)
    }

    fn conn(port: u16) -> ConnArgs {
        ConnArgs {
            host: "127.0.0.1".to_owned(),
            port,
            user: "default".to_owned(),
            database: "default".to_owned(),
            password: Some(String::new()),
        }
    }

    /// Waits for the server to have taken `want` connections, so a count
    /// read too early cannot pass for a retry that did not happen.
    async fn settled(taken: &AtomicUsize, want: usize) -> usize {
        for _ in 0..200 {
            if taken.load(Ordering::SeqCst) >= want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        taken.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn a_read_on_a_closed_connection_goes_out_again() {
        let (port, taken) = serve(vec![None, Some(response("200 OK", &one_row(42), ""))]);
        let value: u32 = conn(port)
            .connect_uncompressed()
            .fetch_one_reconnecting("SELECT 42")
            .await
            .expect("the second connection answers");
        assert_eq!(value, 42);
        assert_eq!(settled(&taken, 2).await, 2, "the read did not go out again");
    }

    #[tokio::test]
    async fn a_read_that_fails_for_another_reason_does_not() {
        let exception = b"Code: 60. DB::Exception: Unknown table. (UNKNOWN_TABLE)";
        let (port, taken) = serve(vec![
            Some(response(
                "500 Internal Server Error",
                exception,
                "X-ClickHouse-Exception-Code: 60\r\n",
            )),
            Some(response("200 OK", &one_row(42), "")),
        ]);
        let error = conn(port)
            .connect_uncompressed()
            .fetch_one_reconnecting::<u32>("SELECT 42")
            .await
            .expect_err("the server refused the statement");
        assert!(error.to_string().contains("UNKNOWN_TABLE"), "{error}");
        assert_eq!(
            settled(&taken, 2).await,
            1,
            "the server's own exception must not be asked again"
        );
    }

    #[tokio::test]
    async fn a_second_closed_connection_is_reported() {
        let (port, taken) = serve(vec![None, None, Some(response("200 OK", &one_row(42), ""))]);
        let error = conn(port)
            .connect_uncompressed()
            .fetch_one_reconnecting::<u32>("SELECT 42")
            .await
            .expect_err("both connections were closed under the read");
        assert!(error.on_a_closed_connection(), "{error}");
        assert_eq!(settled(&taken, 3).await, 2, "the read is asked again once");
    }

    #[tokio::test]
    async fn a_server_that_is_not_there_is_not_a_closed_connection() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a local port");
        let port = listener.local_addr().expect("the bound address").port();
        drop(listener);
        let error = conn(port)
            .connect_uncompressed()
            .fetch_one::<u32>("SELECT 42")
            .await
            .expect_err("nothing is listening there");
        assert!(
            !error.on_a_closed_connection(),
            "an unreachable server is not a stale connection: {error}"
        );
    }
}
