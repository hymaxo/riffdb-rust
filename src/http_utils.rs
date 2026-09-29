// Port of HttpUtils.h / HttpUtils.c

use libc::c_char;

use crate::http_response::{http_response_body, http_response_status_code, HttpResponse};

pub unsafe fn http_utils_res_error(res: *mut HttpResponse, status: u16, err: *const c_char) {
    http_response_status_code(res, status);
    http_response_body(res, libc::strlen(err) as u32, err);
}
