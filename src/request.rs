// A complete request, as handed from the network thread to a worker.
//
// Connection state is split by owner: the network thread keeps the parser
// (`Connection` in socket_actions.rs), and each finished request is moved to
// a worker together with a shared handle to the socket. Workers keep their
// own response buffer and database connection.

use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::http_parser::HTTP_PARSER_URL_SIZE;

pub struct Request {
    /// The URL buffer from the parser, NUL-terminated. The router hashes it
    /// up to the NUL.
    pub url: [u8; HTTP_PARSER_URL_SIZE],
    /// Exactly Content-Length bytes.
    pub body: Vec<u8>,

    /// Shared with the connection (one Arc, so one refcount bump per request).
    pub conn: Arc<ConnShared>,
}

/// The parts of a connection a worker needs.
pub struct ConnShared {
    /// Where the response goes.
    pub client: TcpStream,
    /// Set by the network thread when the client disconnects.
    pub cancel: AtomicBool,
}
