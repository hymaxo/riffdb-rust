use rusqlite::Connection;
use std::sync::atomic::Ordering;

use crate::http_response::HttpResponse;
use crate::log_trace;
use crate::request::Request;
use crate::service::{self, ServiceError};

pub fn execute(req: &Request, db: &Connection, res: &mut HttpResponse) {
    match service::execute(&req.conn.cancel, db, &req.body) {
        Ok(()) if req.conn.cancel.load(Ordering::SeqCst) => {}
        Ok(()) => res.status_and_body(200, b"ok"),
        Err(ServiceError::Failed(msg)) => {
            log_trace!("execute failed: {}", msg);
            res.status_and_body(500, msg.as_bytes());
        }
        Err(ServiceError::Cancelled) => {}
    }
}
