// Port of HttpResponse.h / HttpResponse.c

use libc::{c_char, c_void};
use std::ptr;

use crate::xmalloc::{xfree, xmalloc, xrealloc};

pub const HTTP_RESPONSE_BUFFER_CAPACITY: u32 = 4096;

const HTTP_PROTOCOL_STR: &[u8] = b"HTTP/1.1 ";

/// Formats `v` in decimal into the tail of `buf` and returns the digits
/// (what C does with sprintf("%d") into a stack buffer, without the heap
/// allocation `to_string()` needs).
#[inline]
pub fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    &buf[i..]
}

#[repr(C)]
pub struct HttpResponse {
    pub buf: *mut c_char,
    pub len: u32,
    pub cap: u32,
}

#[inline]
unsafe fn http_response_append(self_: *mut HttpResponse, buffer_size: u32, buffer: *const c_char) {
    // PORT FIX: the C version doubles the capacity only once, so appending a
    // chunk larger than the current capacity (e.g. a big query result)
    // overflows the heap buffer. Keep doubling until it fits.
    while (*self_).len + buffer_size > (*self_).cap {
        (*self_).cap *= 2;
        (*self_).buf = xrealloc((*self_).buf as *mut c_void, (*self_).cap as usize) as *mut c_char;
    }

    ptr::copy_nonoverlapping(buffer, (*self_).buf.add((*self_).len as usize), buffer_size as usize);

    (*self_).len += buffer_size;
}

/// `self_` may point at zeroed / uninitialized memory.
pub unsafe fn http_response_init(self_: *mut HttpResponse) {
    ptr::write(
        self_,
        HttpResponse {
            buf: xmalloc(HTTP_RESPONSE_BUFFER_CAPACITY as usize) as *mut c_char,
            len: 0,
            cap: HTTP_RESPONSE_BUFFER_CAPACITY,
        },
    );
}

pub unsafe fn http_response_zero(self_: *mut HttpResponse) {
    (*self_).len = 0;
}

pub unsafe fn http_response_status_code(self_: *mut HttpResponse, status: u16) {
    let mut digits = [0u8; 20];
    let status_str = fmt_u64(status as u64, &mut digits);

    http_response_append(self_, HTTP_PROTOCOL_STR.len() as u32, HTTP_PROTOCOL_STR.as_ptr() as *const c_char);
    http_response_append(self_, status_str.len() as u32, status_str.as_ptr() as *const c_char);
    http_response_append(self_, 2, c"\r\n".as_ptr());
}

#[allow(dead_code)] // C API; http_response_body now writes its header inline
pub unsafe fn http_response_header(self_: *mut HttpResponse, key: *const c_char, value: *const c_char) {
    http_response_append(self_, libc::strlen(key) as u32, key);
    http_response_append(self_, 2, c": ".as_ptr());
    http_response_append(self_, libc::strlen(value) as u32, value);
    http_response_append(self_, 2, c"\r\n".as_ptr());
}

pub unsafe fn http_response_body(self_: *mut HttpResponse, len: u32, body: *const c_char) {
    // HttpResponseHeader(Self, "Content-Length", LenStr), with both lengths
    // known up front instead of strlen'd.
    let mut digits = [0u8; 20];
    let len_str = fmt_u64(len as u64, &mut digits);

    const KEY: &[u8] = b"Content-Length: ";
    http_response_append(self_, KEY.len() as u32, KEY.as_ptr() as *const c_char);
    http_response_append(self_, len_str.len() as u32, len_str.as_ptr() as *const c_char);
    http_response_append(self_, 2, c"\r\n".as_ptr());
    http_response_append(self_, 2, c"\r\n".as_ptr());
    http_response_append(self_, len, body);
}

pub unsafe fn http_response_free(self_: *mut HttpResponse) {
    xfree((*self_).buf as *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn fmt_u64_matches_to_string() {
        let mut buf = [0u8; 20];
        for v in [0u64, 1, 9, 10, 200, 404, 65535, 4294967295, u64::MAX] {
            assert_eq!(fmt_u64(v, &mut buf), v.to_string().as_bytes());
        }
    }

    unsafe fn bytes<'a>(r: *const HttpResponse) -> &'a [u8] {
        std::slice::from_raw_parts((*r).buf as *const u8, (*r).len as usize)
    }

    #[test]
    fn wire_format_matches_c() {
        unsafe {
            let mut res = MaybeUninit::<HttpResponse>::uninit();
            let r = res.as_mut_ptr();
            http_response_init(r);

            http_response_status_code(r, 200);
            http_response_body(r, 6, c"health".as_ptr());
            assert_eq!(bytes(r), b"HTTP/1.1 200\r\nContent-Length: 6\r\n\r\nhealth");

            http_response_zero(r);
            http_response_status_code(r, 404);
            http_response_body(r, 0, c"".as_ptr());
            assert_eq!(bytes(r), b"HTTP/1.1 404\r\nContent-Length: 0\r\n\r\n");

            // Larger than two doublings of the initial capacity (PORT FIX 1).
            http_response_zero(r);
            let big = vec![b'x' as c_char; 100_000];
            http_response_status_code(r, 200);
            http_response_body(r, big.len() as u32, big.as_ptr());
            let head = b"HTTP/1.1 200\r\nContent-Length: 100000\r\n\r\n";
            assert_eq!(&bytes(r)[..head.len()], head);
            assert_eq!(bytes(r).len(), head.len() + 100_000);

            http_response_free(r);
        }
    }
}
