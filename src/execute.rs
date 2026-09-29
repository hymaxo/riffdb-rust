// Port of Execute.h / Execute.c

use libc::c_void;
use std::ptr;
use std::sync::atomic::Ordering;

use crate::http_response::{http_response_body, http_response_status_code};
use crate::http_utils::http_utils_res_error;
use crate::log_trace;
use crate::request::Request;
use crate::service::{service_execute, ServiceState, SERVICE_ERROR_CANCEL, SERVICE_ERROR_SQLITE, SERVICE_OK};
use crate::xmalloc::xfree;

pub unsafe fn execute(req: *mut Request) {
    let res = ptr::addr_of_mut!((*req).state.response);

    let mut state = ServiceState {
        cancel: ptr::addr_of!((*req).cancel),
        db: (*req).worker.db,
        payload: (*req).state.parser.body,
        payload_len: (*req).state.parser.content_length,
        res_size: 0,
        res: ptr::null(),
        status: 0,
        res_buf: Vec::new(),
    };

    let rc = service_execute(&mut state);
    if rc != SERVICE_OK {
        log_trace!("Rc = {}", rc);
        if rc != SERVICE_ERROR_CANCEL {
            http_utils_res_error(res, state.status, state.res);
            // PORT FIX: in C this free sits after the `return` and is
            // unreachable, so every sqlite error message leaked.
            if rc == SERVICE_ERROR_SQLITE {
                xfree(state.res as *mut c_void);
            }
            return;
        }
    }

    if (*req).cancel.load(Ordering::SeqCst) {
        return;
    }

    http_response_status_code(res, state.status);
    http_response_body(res, state.res_size, state.res);
}
