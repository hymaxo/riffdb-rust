// Port of Execute.h / Execute.c

use rusqlite::Connection;
use std::sync::atomic::Ordering;

use crate::http_response::HttpResponse;
use crate::http_utils::http_utils_res_error;
use crate::log_trace;
use crate::request::Request;
use crate::service::{service_execute, ServiceState, SERVICE_ERROR_CANCEL, SERVICE_OK};

pub fn execute(req: &Request, db: &Connection, res: &mut HttpResponse) {
    let mut no_json = Vec::new(); // /execute never writes JSON
    let mut state = ServiceState::new(&req.conn.cancel, db, &req.body, &mut no_json);

    let rc = service_execute(&mut state);
    if rc != SERVICE_OK {
        log_trace!("Rc = {}", rc);
        if rc != SERVICE_ERROR_CANCEL {
            http_utils_res_error(res, state.status, state.res());
            return;
        }
    }

    if req.conn.cancel.load(Ordering::SeqCst) {
        return;
    }

    res.status_code(state.status);
    res.body(state.res());
}
