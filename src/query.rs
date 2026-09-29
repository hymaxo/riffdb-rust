// Port of Query.h / Query.c

use rusqlite::Connection;
use std::sync::atomic::Ordering;

use crate::http_response::HttpResponse;
use crate::http_utils::http_utils_res_error;
use crate::log_trace;
use crate::request::Request;
use crate::service::{service_query, ServiceState, SERVICE_ERROR_CANCEL, SERVICE_OK};

pub fn query(req: &Request, db: &Connection, res: &mut HttpResponse) {
    // The JSON is written straight into the response (C: yyjson buffer, then
    // copied by HttpResponseBody).
    let mut state = ServiceState::new(&req.conn.cancel, db, &req.body, res.begin_body_in_place());

    let rc = service_query(&mut state);
    let status = state.status;
    if rc != SERVICE_OK {
        log_trace!("Rc = {}", rc);
        let err = state.res().to_vec();
        res.zero();
        if rc != SERVICE_ERROR_CANCEL {
            http_utils_res_error(res, status, &err);
        }
        return;
    }

    if req.conn.cancel.load(Ordering::SeqCst) {
        res.zero();
        return;
    }

    res.finish_body_in_place(status);
}
