// Port of Request.h
//
// In C one heap `Request` per connection holds the parser, the response
// buffer and the worker's db handle, and is shared between the network
// thread and a worker through a raw pointer. Here it is split by owner:
//
// - `Connection` (socket_actions.rs) stays with the network thread and owns
//   the parser.
// - `Request` (this file) is what a worker receives: everything about one
//   complete request, moved out of the parser, plus a handle to write the
//   response to. The worker keeps its own response buffer and db.

use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::http_parser::HTTP_PARSER_URL_SIZE;

pub struct Request {
    /// Parser.Url as-is: NUL-terminated, the router hashes it like a C string.
    pub url: [u8; HTTP_PARSER_URL_SIZE],
    /// Exactly Content-Length bytes (C: Parser.Body / Parser.ContentLength).
    pub body: Vec<u8>,

    /// Shared with the connection (one Arc, so one refcount bump per request).
    pub conn: Arc<ConnShared>,
}

/// The parts of a connection a worker needs.
pub struct ConnShared {
    /// Where the response goes (C: Req->ClientFd).
    pub client: TcpStream,
    /// Set by the network thread when the client disconnects (C: Req->Cancel).
    pub cancel: AtomicBool,
}
