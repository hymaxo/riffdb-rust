use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::http_parser::HTTP_PARSER_URL_SIZE;

pub struct Request {
    pub url: [u8; HTTP_PARSER_URL_SIZE],
    pub body: Vec<u8>,

    pub conn: Arc<ConnShared>,
}

pub struct ConnShared {
    pub client: TcpStream,
    pub cancel: AtomicBool,
}
