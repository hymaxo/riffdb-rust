use rusqlite::Connection;
use std::sync::atomic::Ordering;

use crate::http_response::HttpResponse;
use crate::log_trace;
use crate::request::Request;
use crate::service::{self, ServiceError};

pub fn query(req: &Request, db: &Connection, res: &mut HttpResponse) {
    match service::query(&req.conn.cancel, db, &req.body, res.begin_body_in_place()) {
        Ok(()) if req.conn.cancel.load(Ordering::SeqCst) => res.zero(),
        Ok(()) => res.finish_body_in_place(200),
        Err(ServiceError::Failed(msg)) => {
            log_trace!("query failed: {}", msg);
            res.zero();
            res.status_and_body(500, msg.as_bytes());
        }
        Err(ServiceError::Cancelled) => res.zero(),
    }
}
