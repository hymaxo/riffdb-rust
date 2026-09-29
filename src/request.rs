// Port of Request.h

use libsqlite3_sys::sqlite3;
use std::sync::atomic::AtomicBool;

use crate::http_parser::HttpParser;
use crate::http_response::HttpResponse;
use crate::sys::Socket;

#[repr(C)]
pub struct RequestState {
    pub parser: HttpParser,
    pub response: HttpResponse,
}

#[repr(C)]
pub struct RequestWorker {
    pub db: *mut sqlite3,
}

#[repr(C)]
pub struct Request {
    pub state: RequestState,
    pub worker: RequestWorker,
    pub client_fd: Socket,
    pub cancel: AtomicBool,
}
