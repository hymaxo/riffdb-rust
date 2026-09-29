// Port of SocketActions.h / SocketActions.c

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::http_parser::{HttpParser, HTTP_PARSER_STATE_BODY, HTTP_PARSER_STATE_COMPLETE};
use crate::log::{self, LogVerbosity};
use crate::log_trace;
use crate::request::{ConnShared, Request};
use crate::tcp_server::{
    TcpServerCallbacks, TCP_SERVER_ERROR_EMPTY_READ, TCP_SERVER_ERROR_READ, TCP_SERVER_ERROR_WOULD_BLOCK,
    TCP_SERVER_READ_SHORT,
};
use crate::thread_pool::ThreadPool;

/// The network thread's per-client state (C: the parser half of Request).
pub struct Connection {
    pub parser: HttpParser,
    /// Write handle + cancel flag, handed to workers with each request.
    pub shared: Arc<ConnShared>,
    fd: u64,
}

/// The OS socket handle, for the trace logs (C logs the fd).
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

/// The C worker's "Full parser dump", done here because the parser now stays
/// with the network thread. Only runs with trace logging on.
#[cold]
#[inline(never)]
fn dump_parser(p: &HttpParser) {
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    log_trace!("=== HttpParser dump ===");
    log_trace!("  State          = {}", p.state);
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

    /// SocketActionsOnConnect
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

    /// SocketActionsOnReadable
    fn on_readable(&mut self, client: &mut mio::net::TcpStream, conn: &mut Connection, buf: &mut [u8]) -> i16 {
        let n = match client.read(buf) {
            Ok(0) => return TCP_SERVER_ERROR_EMPTY_READ,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return TCP_SERVER_ERROR_WOULD_BLOCK,
            Err(e) if e.kind() == ErrorKind::Interrupted => return 0,
            Err(_) => return TCP_SERVER_ERROR_READ,
        };
        let data = &buf[..n];
        let ok = if n < buf.len() { TCP_SERVER_READ_SHORT } else { 0 };

        conn.parser.parse(data);

        if conn.parser.state != HTTP_PARSER_STATE_BODY && conn.parser.state != HTTP_PARSER_STATE_COMPLETE {
            return ok;
        }

        conn.parser.parse_body(data);

        // PORT FIX: only hand complete requests to a worker (C dispatches on
        // every read once the headers are done, body complete or not).
        if conn.parser.state != HTTP_PARSER_STATE_COMPLETE {
            return ok;
        }

        if log::enabled(LogVerbosity::Trace) {
            dump_parser(&conn.parser);
        }

        let req = Request {
            url: conn.parser.url,
            body: conn.parser.take_body(),
            conn: Arc::clone(&conn.shared),
        };

        // A full mailbox drops the request, like Enqueue in C.
        let _ = self.pool.process(req);

        ok
    }

    /// SocketActionsOnDisconnect
    fn on_disconnect(&mut self, conn: Connection) {
        log_trace!("Client disconnected: fd={}", conn.fd);

        // Requests still queued or running see this and skip / stop
        // sending. The socket closes once the last of them is done with it.
        conn.shared.cancel.store(true, Ordering::SeqCst);
    }
}
