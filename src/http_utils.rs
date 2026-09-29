// Port of HttpUtils.h / HttpUtils.c

use crate::http_response::HttpResponse;

pub fn http_utils_res_error(res: &mut HttpResponse, status: u16, err: &[u8]) {
    res.status_code(status);
    res.body(err);
}
