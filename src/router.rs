// Port of Router.h / Router.c

use libc::c_char;
use std::ptr;

use crate::execute::execute;
use crate::http_response::{http_response_body, http_response_status_code};
use crate::query::query;
use crate::request::Request;

// k&r style shit...
// Sums the chars (signed, as `char` is in C on x86) up to the first NUL, or
// the end of the slice. The NUL itself adds 0, so it is simply not counted.
const fn hash(str: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    let mut i = 0;
    while i < str.len() && str[i] != 0 {
        hash = hash.wrapping_add(str[i] as i8 as i32 as u32);
        i += 1;
    }
    hash
}

// C computed these at startup in RouterInit(); here they are compile-time.
const EXECUTE_ROUTE: u32 = hash(b"/execute");
const QUERY_ROUTE: u32 = hash(b"/query");
const HEALTH_ROUTE: u32 = hash(b"/health");

pub unsafe fn router_route(req: *mut Request) {
    let res = ptr::addr_of_mut!((*req).state.response);

    let url = &(*req).state.parser.url;
    let route = hash(std::slice::from_raw_parts(url.as_ptr() as *const u8, url.len()));

    if route == EXECUTE_ROUTE {
        execute(req);
        return;
    }
    if route == QUERY_ROUTE {
        query(req);
        return;
    }
    if route == HEALTH_ROUTE {
        let body = b"health";
        http_response_status_code(res, 200);
        http_response_body(res, body.len() as u32, body.as_ptr() as *const c_char);
        return;
    }

    let body = b"not found";
    http_response_status_code(res, 404);
    http_response_body(res, body.len() as u32, body.as_ptr() as *const c_char);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_c_semantics() {
        assert_eq!(hash(b"/query"), b"/query".iter().map(|&c| c as u32).sum::<u32>());
        // stops at NUL, like the C loop over a C string
        assert_eq!(hash(b"/query\0garbage"), QUERY_ROUTE);
        // signed char: bytes >= 0x80 subtract
        assert_eq!(hash(&[0xff]), (-1i32) as u32);
        // known collision in the original scheme: any permutation matches
        assert_eq!(hash(b"/yreuq"), QUERY_ROUTE);
        assert_ne!(EXECUTE_ROUTE, QUERY_ROUTE);
        assert_ne!(QUERY_ROUTE, HEALTH_ROUTE);
    }
}
