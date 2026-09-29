// The network thread's side of a connection: read, parse, and hand complete
// requests to the worker pool.

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::http_parser::{HttpParser, ParserState};
use crate::log::{self, LogVerbosity};
use crate::log_trace;
use crate::request::{ConnShared, Request};
use crate::tcp_server::{ReadStatus, TcpServerCallbacks};
use crate::thread_pool::ThreadPool;

/// Per-client state owned by the network thread.
pub struct Connection {
    pub parser: HttpParser,
    /// Write handle + cancel flag, handed to workers with each request.
    pub shared: Arc<ConnShared>,
    fd: u64,
}

/// The OS socket handle, for the trace logs.
fn raw_fd(s: &std::net::TcpStream) -> u64 {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s) as u64
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

/// Logs the parser state for a finished request. Only runs with trace
/// logging on.
#[cold]
#[inline(never)]
fn dump_parser(p: &HttpParser) {
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    log_trace!("=== HttpParser dump ===");
    log_trace!("  State          = {:?}", p.state);
    log_trace!("  SawCr          = {}", p.saw_cr as i32);
    log_trace!("  SawDoubleDot   = {}", p.saw_double_dot as i32);
    log_trace!("  Method         = {} (len={})", s(p.method()), p.method_len);
    log_trace!("  Url            = {} (len={})", s(p.url()), p.url_len);
    log_trace!("  HeadersLen     = {}", p.headers_len);
    for (i, h) in p.headers[..p.headers_len as usize].iter().enumerate() {
        log_trace!("  Header[{}]      = {}: {}", i, s(h.key()), s(&h.value));
    }
    log_trace!("  BodyStart      = {}", p.body_start);
    log_trace!("  BodyCap        = {}", p.body.capacity());
    log_trace!("  ConsumedBody   = {}", p.body.len());
    log_trace!("  ContentLength  = {}", p.content_length);
    if p.content_length > 0 {
        log_trace!("  Body           = {}", s(&p.body));
    } else {
        log_trace!("  Body           = (null or empty)");
    }
    log_trace!("=== end HttpParser dump ===");
}

pub struct SocketActions<'a> {
    pub pool: &'a ThreadPool<Request>,
}

impl TcpServerCallbacks for SocketActions<'_> {
    type ClientData = Connection;

    fn on_connect(&mut self, client: &std::net::TcpStream) -> Option<Connection> {
        let fd = raw_fd(client);
        log_trace!("Client connected: fd={}", fd);

        Some(Connection {
            parser: HttpParser::new(),
            shared: Arc::new(ConnShared {
                client: client.try_clone().ok()?,
                cancel: AtomicBool::new(false),
            }),
            fd,
        })
    }

    fn on_readable(&mut self, client: &mut mio::net::TcpStream, conn: &mut Connection, buf: &mut [u8]) -> ReadStatus {
        let n = match client.read(buf) {
            Ok(0) => return ReadStatus::Closed,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return ReadStatus::WouldBlock,
            Err(e) if e.kind() == ErrorKind::Interrupted => return ReadStatus::More,
            Err(_) => return ReadStatus::Closed,
        };
        let data = &buf[..n];
        let status = if n < buf.len() { ReadStatus::Short } else { ReadStatus::More };

        conn.parser.parse(data);

        if conn.parser.state != ParserState::Body && conn.parser.state != ParserState::Complete {
            return status;
        }

        conn.parser.parse_body(data);

        // Wait for the whole body before dispatching.
        if conn.parser.state != ParserState::Complete {
            return status;
        }

        if log::enabled(LogVerbosity::Trace) {
            dump_parser(&conn.parser);
        }

        let req = Request {
            url: conn.parser.url,
            body: conn.parser.take_body(),
            conn: Arc::clone(&conn.shared),
        };

        // A full mailbox (1024 queued requests) drops the request.
        let _ = self.pool.process(req);

        status
    }

    fn on_disconnect(&mut self, conn: Connection) {
        log_trace!("Client disconnected: fd={}", conn.fd);

        // Requests still queued or running see this and skip / stop
        // sending. The socket closes once the last of them is done with it.
        conn.shared.cancel.store(true, Ordering::SeqCst);
    }
}
