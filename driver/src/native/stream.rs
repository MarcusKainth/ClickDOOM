//! One `INSERT ... SELECT ... FROM input(...)` held open for a whole
//! session, with one row streamed into it per tic.
//!
//! The statement text leads the request body and the rows follow it, so a
//! statement larger than a URL parameter still opens. The server reads
//! `max_query_size` bytes before it parses, so [`Resident::open`] writes a
//! padding row behind the statement; the statement drops it. Every later
//! row is one chunk of the body and is processed as it arrives.
//!
//! A statement's error reaches the response only once the body closes. A
//! statement the server has already given up on goes on taking rows and
//! writing none of them, so what shows a caller that it died is its rows no
//! longer landing: close it and the error is there. The response is read
//! from the moment the request is sent, so a response that does arrive
//! earlier is recorded the same way.
//!
//! The server reads the request body to its end before it answers, and one
//! of its own connections is busy for the whole of that. [`Resident::close`]
//! therefore takes a bound, and drops the connection when it passes.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration; // purity-ok: a bound on a wait in the driver loop, never a value a statement reads

use bytes::{BufMut, Bytes, BytesMut};
use http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use super::rowbinary::{self, SchemaError};
use super::url;
use crate::client::ConnArgs;

/// The format clause [`Resident::open`] appends to the statement. The rows
/// this module encodes are RowBinary, so the statement says so rather than
/// the caller.
pub const FORMAT_CLAUSE: &str = " FORMAT RowBinary";

/// The header ClickHouse puts an exception code in.
const EXCEPTION_CODE: &str = "x-clickhouse-exception-code";

/// How much of a server message an error carries.
const MESSAGE_CHARS: usize = 1000;

/// The bound the driver gives [`Resident::close`]. The server answers
/// within a round trip of the last byte of the body, so anything past this
/// is a server that has stopped answering.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Anything that stops a resident statement from opening or from taking
/// another row.
#[derive(Debug, thiserror::Error)]
pub enum ResidentError {
    #[error("connecting to {addr}: {source}. Is ClickHouse listening there?")]
    Connect {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("HTTP handshake with {addr}: {source}")]
    Handshake {
        addr: String,
        #[source]
        source: hyper::Error,
    },
    #[error("building the request for {addr}: {source}")]
    Request {
        addr: String,
        #[source]
        source: http::Error,
    },
    #[error(
        "the statement already ends in a FORMAT clause; leave it off, \
         the transport appends `{FORMAT_CLAUSE}`"
    )]
    OwnFormat,
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// The statement is no longer running. `status` is the HTTP status when
    /// the server answered, absent when the connection failed first.
    #[error("the resident statement ended: {message}")]
    Ended {
        status: Option<u16>,
        message: String,
    },
    /// The body closed and no response followed inside the bound
    /// [`Resident::close`] was given. The connection is dropped, so the
    /// server stops reading the body.
    #[error("the server did not answer within {waited:?} of the body closing")]
    Unanswered { waited: Duration },
    #[error("the task reading the response panicked: {source}")]
    Watcher {
        #[source]
        source: tokio::task::JoinError,
    },
}

/// Why a resident statement stopped, as the response task recorded it.
#[derive(Clone, Debug)]
struct Ended {
    status: Option<u16>,
    message: String,
}

impl From<&Ended> for ResidentError {
    fn from(ended: &Ended) -> ResidentError {
        ResidentError::Ended {
            status: ended.status,
            message: ended.message.clone(),
        }
    }
}

/// The request body: one chunk per row [`Resident::send`] hands over.
struct RowBody {
    rows: UnboundedReceiver<Bytes>,
}

impl Body for RowBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .rows
            .poll_recv(cx)
            .map(|row| row.map(|row| Ok(Frame::data(row))))
    }
}

/// One statement, open for as long as the session lasts.
///
/// Rows are queued without bound, because the caller sends the row for tic
/// `t + 1` only once tic `t` is readable, which holds the queue at one row.
pub struct Resident {
    rows: UnboundedSender<Bytes>,
    outcome: Arc<OnceLock<Result<(), Ended>>>,
    response: JoinHandle<()>,
    connection: JoinHandle<()>,
}

impl Resident {
    /// Opens the statement and writes the padding row behind it.
    ///
    /// `input_schema` is the schema the statement's own `input(...)`
    /// declares; it decides the padding row's encoding, so the two have to
    /// agree. `settings` travel as URL parameters:
    /// [`resident_settings`](super::settings::resident_settings) produces
    /// the set a resident statement needs, and a caller can append its own,
    /// `query_id` among them.
    ///
    /// Returning `Ok` means the request is on the wire, not that the
    /// server accepted the statement. A rejected statement surfaces
    /// through [`send`](Resident::send) or [`close`](Resident::close).
    pub async fn open(
        conn: &ConnArgs,
        statement: &str,
        input_schema: &str,
        settings: &[(&str, String)],
    ) -> Result<Resident, ResidentError> {
        if ends_in_a_format_clause(statement) {
            return Err(ResidentError::OwnFormat);
        }
        let padding = rowbinary::padding_row(input_schema)?;
        let addr = format!("{}:{}", conn.host, conn.port);

        let socket = TcpStream::connect(&addr)
            .await
            .map_err(|source| ResidentError::Connect {
                addr: addr.clone(),
                source,
            })?;
        // Every row is one small write. Waiting to coalesce it with the
        // next one costs a tic.
        socket
            .set_nodelay(true)
            .map_err(|source| ResidentError::Connect {
                addr: addr.clone(),
                source,
            })?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .map_err(|source| ResidentError::Handshake {
                addr: addr.clone(),
                source,
            })?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        sender
            .ready()
            .await
            .map_err(|source| ResidentError::Handshake {
                addr: addr.clone(),
                source,
            })?;

        let (rows, receiver) = mpsc::unbounded_channel();
        let head = head_frame(statement, &padding);
        rows.send(head).map_err(|_| ResidentError::Ended {
            status: None,
            message: "the request body was dropped before the statement was written".to_owned(),
        })?;

        let request = Request::builder()
            .method("POST")
            .uri(url::request_target(&conn.database, settings))
            .header(http::header::HOST, &addr)
            .header("X-ClickHouse-User", &conn.user)
            .header("X-ClickHouse-Key", conn.resolved_password())
            .body(RowBody { rows: receiver })
            .map_err(|source| ResidentError::Request {
                addr: addr.clone(),
                source,
            })?;

        let sending = sender.send_request(request);
        let outcome = Arc::new(OnceLock::new());
        let sink = Arc::clone(&outcome);
        let response = tokio::spawn(async move {
            let ended = read_response(sending).await;
            // Holding the sender until here keeps hyper from tearing the
            // connection down while the body is still being written.
            drop(sender);
            let _ = sink.set(ended);
        });

        Ok(Resident {
            rows,
            outcome,
            response,
            connection,
        })
    }

    /// Queues one RowBinary row as the next chunk of the body.
    ///
    /// A row this accepts is not committed, and a statement that has
    /// already failed still accepts rows: read the destination table to
    /// know one landed. `Err` here means the response has arrived, which
    /// for a failure is usually only after [`close`](Resident::close).
    pub fn send(&self, row: Bytes) -> Result<(), ResidentError> {
        if let Some(outcome) = self.outcome.get() {
            return Err(finished(outcome));
        }
        self.rows.send(row).map_err(|_| ResidentError::Ended {
            status: None,
            message: "the server stopped reading the request body".to_owned(),
        })
    }

    /// Whether the response is still outstanding. A statement the server
    /// has abandoned reads as alive until its body closes.
    pub fn alive(&self) -> bool {
        self.outcome.get().is_none()
    }

    /// Ends the body and reports what the server said, waiting at most
    /// `bound` for it.
    ///
    /// `Ok` means the server took every row. A server that stops answering
    /// leaves one of its connections reading a body nothing is going to
    /// finish, so when `bound` passes the connection is dropped and the
    /// call reports [`ResidentError::Unanswered`].
    pub async fn close(self, bound: Duration) -> Result<(), ResidentError> {
        let Resident {
            rows,
            outcome,
            mut response,
            connection,
        } = self;
        drop(rows);
        let joined = tokio::time::timeout(bound, &mut response).await;
        // The connection goes either way. When the bound passes, dropping
        // it is what ends the server's read of the body.
        connection.abort();
        let Ok(joined) = joined else {
            response.abort();
            return Err(ResidentError::Unanswered { waited: bound });
        };
        joined.map_err(|source| ResidentError::Watcher { source })?;
        match outcome.get() {
            Some(Err(ended)) => Err(ended.into()),
            _ => Ok(()),
        }
    }
}

/// The error a [`Resident`] whose outcome is already recorded reports.
fn finished(outcome: &Result<(), Ended>) -> ResidentError {
    match outcome {
        Ok(()) => ResidentError::Ended {
            status: None,
            message: "the statement already finished".to_owned(),
        },
        Err(ended) => ended.into(),
    }
}

/// The statement, its format clause, a newline, then the padding row.
fn head_frame(statement: &str, padding: &Bytes) -> Bytes {
    let mut head =
        BytesMut::with_capacity(statement.len() + FORMAT_CLAUSE.len() + 1 + padding.len());
    head.put_slice(statement.as_bytes());
    head.put_slice(FORMAT_CLAUSE.as_bytes());
    head.put_u8(b'\n');
    head.put_slice(padding);
    head.freeze()
}

/// Whether the last two words of `statement` are `FORMAT <name>`.
fn ends_in_a_format_clause(statement: &str) -> bool {
    let mut words = statement.split_whitespace().rev();
    words.next().is_some()
        && words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("FORMAT"))
}

/// Awaits the response and reads its body to the end.
async fn read_response(
    sending: impl Future<Output = Result<Response<Incoming>, hyper::Error>>,
) -> Result<(), Ended> {
    let response = match sending.await {
        Ok(response) => response,
        Err(source) => {
            return Err(Ended {
                status: None,
                message: format!("the connection failed before a response arrived: {source}"),
            });
        }
    };
    let status = response.status();
    let code = response
        .headers()
        .get(EXCEPTION_CODE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match response.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(source) => {
            return Err(Ended {
                status: Some(status.as_u16()),
                message: format!("HTTP {}, then the body failed: {source}", status.as_u16()),
            });
        }
    };
    classify(status, code.as_deref(), &body)
}

/// Reads the response as success or as the statement's failure.
///
/// A statement that fails after the server has sent its headers answers
/// 200 with the exception appended to the body, so the body is checked
/// whatever the status says.
fn classify(status: StatusCode, code: Option<&str>, body: &[u8]) -> Result<(), Ended> {
    let text = String::from_utf8_lossy(body);
    if status.is_success() && code.is_none() && exception_text(&text).is_none() {
        return Ok(());
    }
    let mut message = match code {
        Some(code) => format!("HTTP {}, exception code {code}", status.as_u16()),
        None => format!("HTTP {}", status.as_u16()),
    };
    let detail = exception_text(&text).unwrap_or_else(|| text.trim());
    if !detail.is_empty() {
        message.push_str(": ");
        message.push_str(&head(detail, MESSAGE_CHARS));
    }
    Err(Ended {
        status: Some(status.as_u16()),
        message,
    })
}

/// The ClickHouse exception in `body`, from its code or its class name,
/// whichever comes first.
fn exception_text(body: &str) -> Option<&str> {
    let at = ["Code: ", "DB::Exception"]
        .iter()
        .filter_map(|marker| body.find(marker))
        .min()?;
    Some(body[at..].trim())
}

/// The first `chars` characters of `text`, with an ellipsis when it is
/// longer.
fn head(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    use super::super::settings::QUERY_SIZE_SLACK;
    use super::*;

    #[tokio::test]
    async fn a_server_that_says_nothing_ends_the_close_at_its_bound() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("a local port");
        let port = listener.local_addr().expect("the bound address").port();
        // Takes the request, reads it to the end and answers nothing, which
        // is a server that has stopped answering.
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the client connects");
            let mut request = Vec::new();
            socket
                .read_to_end(&mut request)
                .await
                .expect("the client hangs up");
            request.len()
        });

        let conn = ConnArgs {
            host: "127.0.0.1".to_owned(),
            port,
            user: "default".to_owned(),
            database: "default".to_owned(),
            password: Some(String::new()),
        };
        let resident = Resident::open(
            &conn,
            "INSERT INTO t SELECT tic",
            "tic UInt32, pad String",
            &[],
        )
        .await
        .expect("the request goes out");

        let bound = Duration::from_millis(200);
        let error = resident
            .close(bound)
            .await
            .expect_err("a server that says nothing cannot end the statement");
        assert!(
            matches!(error, ResidentError::Unanswered { waited } if waited == bound),
            "{error}"
        );

        let read = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the close has to hang up, so the server sees the end of the body")
            .expect("the serving task");
        assert!(read > 0, "the server saw none of the request");
    }

    #[test]
    fn the_query_size_slack_covers_the_appended_format_clause() {
        assert!(
            QUERY_SIZE_SLACK > FORMAT_CLAUSE.len() + 1,
            "max_query_size has to reach past the statement, its FORMAT clause and the newline"
        );
    }

    #[test]
    fn the_head_frame_is_the_statement_then_a_newline_then_the_padding() {
        let padding = Bytes::from_static(b"\x02ab");
        let head = head_frame("SELECT 1", &padding);
        assert_eq!(
            head,
            Bytes::from_static(b"SELECT 1 FORMAT RowBinary\n\x02ab")
        );
    }

    #[test]
    fn a_statement_carrying_its_own_format_clause_is_refused() {
        assert!(ends_in_a_format_clause(
            "INSERT INTO t SELECT 1 FORMAT RowBinary"
        ));
        assert!(ends_in_a_format_clause(
            "INSERT INTO t SELECT 1 format JSON"
        ));
        assert!(!ends_in_a_format_clause("INSERT INTO t SELECT 1"));
        assert!(!ends_in_a_format_clause("SELECT 'FORMAT'"));
        assert!(!ends_in_a_format_clause(""));
    }

    #[test]
    fn an_empty_two_hundred_is_the_statement_finishing() {
        assert!(classify(StatusCode::OK, None, b"").is_ok());
    }

    #[test]
    fn a_five_hundred_carries_the_servers_own_message() {
        let body = b"Code: 62. DB::Exception: Syntax error: failed at position 62 \
                     (end of query). (SYNTAX_ERROR) (version 26.7.5.10)\n";
        let ended = classify(StatusCode::INTERNAL_SERVER_ERROR, Some("62"), body)
            .expect_err("a 500 is a failure");
        assert_eq!(ended.status, Some(500));
        assert!(
            ended.message.contains("exception code 62"),
            "{}",
            ended.message
        );
        assert!(ended.message.contains("Syntax error"), "{}", ended.message);
    }

    #[test]
    fn an_exception_appended_to_a_two_hundred_is_still_a_failure() {
        let body = b"Code: 241. DB::Exception: Memory limit exceeded. (MEMORY_LIMIT_EXCEEDED)";
        let ended = classify(StatusCode::OK, None, body).expect_err("the body carries a failure");
        assert_eq!(ended.status, Some(200));
        assert!(
            ended.message.contains("Memory limit exceeded"),
            "{}",
            ended.message
        );
    }

    #[test]
    fn a_failure_with_no_body_still_names_its_status() {
        let ended = classify(StatusCode::BAD_REQUEST, None, b"").expect_err("a 400 is a failure");
        assert_eq!(ended.message, "HTTP 400");
    }

    #[test]
    fn the_exception_is_cut_out_of_whatever_precedes_it() {
        let body = "some prefix the server wrote\nCode: 62. DB::Exception: Syntax error";
        assert_eq!(
            exception_text(body),
            Some("Code: 62. DB::Exception: Syntax error")
        );
        assert_eq!(exception_text("nothing here"), None);
    }

    #[test]
    fn a_long_message_is_cut_at_a_character_boundary() {
        let text = "é".repeat(2000);
        let cut = head(&text, MESSAGE_CHARS);
        assert_eq!(cut.chars().count(), MESSAGE_CHARS + 3);
        assert!(cut.ends_with("..."));
        assert_eq!(head("short", MESSAGE_CHARS), "short");
    }

    #[test]
    fn a_send_after_a_clean_finish_reports_the_statement_is_over() {
        let error = finished(&Ok(()));
        assert!(
            matches!(error, ResidentError::Ended { status: None, .. }),
            "{error}"
        );
    }
}
