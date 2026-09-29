// Port of HttpParser.h / HttpParser.c
//
// Incremental HTTP/1.1 request parser. Header values and the body live in
// malloc'd buffers owned through raw pointers, same as the C version.

use libc::{c_char, c_void};
use std::ptr;

use crate::xmalloc::{xfree, xmalloc, xrealloc};

pub type HttpParserError = i32;
#[allow(dead_code)]
pub const HTTP_PARSER_ERROR_ALLOC: HttpParserError = -1;
pub const HTTP_PARSER_ERROR_INCORRECT_STATE: HttpParserError = -2;

pub const HTTP_PARSER_METHOD_SIZE: usize = 8;
pub const HTTP_PARSER_URL_SIZE: usize = 64;
pub const HTTP_PARSER_HEADER_SIZE: usize = 24;
pub const HTTP_PARSER_HEADER_KEY_SIZE: usize = 64;
pub const HTTP_PARSER_HEADER_VALUE_SIZE: usize = 8192;
pub const HTTP_PARSER_BODY_SIZE: usize = 2048;

pub const HTTP_PARSER_STATE_METHOD: u8 = 0;
pub const HTTP_PARSER_STATE_URL: u8 = 1;
pub const HTTP_PARSER_STATE_VERSION: u8 = 2;
pub const HTTP_PARSER_STATE_HEADER_KEY: u8 = 3;
pub const HTTP_PARSER_STATE_HEADER_VALUE: u8 = 4;
pub const HTTP_PARSER_STATE_BODY: u8 = 5;
pub const HTTP_PARSER_STATE_COMPLETE: u8 = 6;

#[repr(C)]
pub struct HttpHeader {
    pub key: [c_char; HTTP_PARSER_HEADER_KEY_SIZE],
    pub value: *mut c_char,
    pub value_len: u16,
    pub key_len: u8,
}

#[repr(C)]
pub struct HttpParser {
    pub state: u8,
    pub saw_cr: bool,
    pub saw_double_dot: bool,

    pub method: [c_char; HTTP_PARSER_METHOD_SIZE],
    pub method_len: u8,

    pub url: [c_char; HTTP_PARSER_URL_SIZE],
    pub url_len: u8,

    pub headers_len: u8,
    pub headers: [HttpHeader; HTTP_PARSER_HEADER_SIZE],

    pub body: *mut c_char,
    pub body_start: u32,
    pub body_cap: u32,
    pub consumed_body: u32,
    pub content_length: u32,
}

pub unsafe fn http_parser_set_content_length(parser: *mut HttpParser) -> u32 {
    // TODO: ssleert - add check for any method except POST or PUT
    //       and return early

    for i in 0..(*parser).headers_len as usize {
        let h = ptr::addr_of_mut!((*parser).headers[i]);
        if (*h).key_len == 14
            && (libc::strncmp((*h).key.as_ptr(), c"Content-Length".as_ptr(), 14) == 0
                || libc::strncmp((*h).key.as_ptr(), c"content-length".as_ptr(), 14) == 0)
        {
            (*parser).content_length = libc::strtoul((*h).value, ptr::null_mut(), 10) as u32;
            return (*parser).content_length;
        }
    }

    0
}

/// `self_` may point at zeroed / uninitialized memory.
pub unsafe fn http_parser_init(self_: *mut HttpParser) -> HttpParserError {
    ptr::write_bytes(self_, 0, 1);

    for i in 0..HTTP_PARSER_HEADER_SIZE {
        (*self_).headers[i].value = xmalloc(HTTP_PARSER_HEADER_VALUE_SIZE) as *mut c_char;
    }

    (*self_).body = xmalloc(HTTP_PARSER_BODY_SIZE) as *mut c_char;

    (*self_).body_cap = HTTP_PARSER_BODY_SIZE as u32;

    0
}

pub unsafe fn http_parser_zero(self_: *mut HttpParser) -> HttpParserError {
    (*self_).state = HTTP_PARSER_STATE_METHOD;
    (*self_).saw_cr = false;
    (*self_).saw_double_dot = false;
    (*self_).method_len = 0;
    (*self_).url_len = 0;
    (*self_).content_length = 0;
    (*self_).consumed_body = 0;
    (*self_).content_length = 0;

    for i in 0..(*self_).headers_len as usize {
        (*self_).headers[i].key_len = 0;
        (*self_).headers[i].value_len = 0;
    }

    (*self_).headers_len = 0;

    0
}

pub unsafe fn http_parser_parse(self_: *mut HttpParser, len: usize, data: *const c_char) -> HttpParserError {
    if (*self_).state == HTTP_PARSER_STATE_BODY {
        return 0;
    }

    if (*self_).state == HTTP_PARSER_STATE_COMPLETE {
        http_parser_zero(self_);
    }

    let s = &mut *self_;

    for i in 0..len {
        let byte = *data.add(i);

        match s.state {
            HTTP_PARSER_STATE_METHOD => {
                if byte == b' ' as c_char {
                    s.method[s.method_len as usize] = 0;
                    s.state = HTTP_PARSER_STATE_URL;
                    continue;
                }

                if s.method_len as usize >= HTTP_PARSER_METHOD_SIZE - 1 {
                    continue;
                }

                s.method[s.method_len as usize] = byte;
                s.method_len += 1;
            }
            HTTP_PARSER_STATE_URL => {
                if byte == b' ' as c_char {
                    s.url[s.url_len as usize] = 0;
                    s.state = HTTP_PARSER_STATE_VERSION;
                    continue;
                }

                if s.url_len as usize >= HTTP_PARSER_URL_SIZE - 1 {
                    continue;
                }

                s.url[s.url_len as usize] = byte;
                s.url_len += 1;
            }
            HTTP_PARSER_STATE_VERSION => {
                if byte == b'\r' as c_char {
                    s.saw_cr = true;
                    continue;
                }

                if byte == b'\n' as c_char && s.saw_cr {
                    s.state = HTTP_PARSER_STATE_HEADER_KEY;
                    s.saw_cr = false;
                    continue;
                }

                // i dont care about version of http
            }
            HTTP_PARSER_STATE_HEADER_KEY => {
                if byte == b':' as c_char {
                    s.saw_double_dot = true;
                    continue;
                }

                if byte == b' ' as c_char && s.saw_double_dot {
                    // PORT NOTE: C writes out of bounds of Headers[] once more
                    // than HttpParserHeaderSize headers arrive; here the array
                    // index is bounds-checked and will panic instead.
                    let h = &mut s.headers[s.headers_len as usize];
                    h.key[h.key_len as usize] = 0;
                    s.state = HTTP_PARSER_STATE_HEADER_VALUE;
                    s.saw_double_dot = false;
                    continue;
                }

                if byte == b'\r' as c_char {
                    s.saw_cr = true;
                    continue;
                }

                if byte == b'\n' as c_char && s.saw_cr {
                    s.saw_cr = false;

                    http_parser_set_content_length(self_);
                    let s = &mut *self_;
                    if s.content_length == 0 {
                        s.state = HTTP_PARSER_STATE_COMPLETE;
                        return 0;
                    }

                    s.state = HTTP_PARSER_STATE_BODY;
                    s.body_start = (i + 1) as u32;
                    return 0;
                }

                if s.headers_len as usize >= HTTP_PARSER_HEADER_SIZE - 1 {
                    continue;
                }

                let h = &mut s.headers[s.headers_len as usize];
                if h.key_len as usize >= HTTP_PARSER_HEADER_KEY_SIZE - 1 {
                    continue;
                }

                h.key[h.key_len as usize] = byte;
                h.key_len += 1;
            }
            HTTP_PARSER_STATE_HEADER_VALUE => {
                if byte == b'\r' as c_char {
                    s.saw_cr = true;
                    continue;
                }

                if byte == b'\n' as c_char && s.saw_cr {
                    let h = &mut s.headers[s.headers_len as usize];
                    *h.value.add(h.value_len as usize) = 0;
                    s.state = HTTP_PARSER_STATE_HEADER_KEY;
                    s.saw_cr = false;
                    s.headers_len += 1;
                    continue;
                }

                let h = &mut s.headers[s.headers_len as usize];
                if h.value_len as usize >= HTTP_PARSER_HEADER_VALUE_SIZE - 1 {
                    continue;
                }

                *h.value.add(h.value_len as usize) = byte;
                h.value_len += 1;
            }
            HTTP_PARSER_STATE_BODY => {
                return 0;
            }
            _ => {
                return HTTP_PARSER_ERROR_INCORRECT_STATE;
            }
        }
    }

    0
}

pub unsafe fn http_parser_parse_body(
    self_: *mut HttpParser,
    mut len: usize,
    mut data: *const c_char,
) -> HttpParserError {
    if (*self_).state != HTTP_PARSER_STATE_BODY {
        return 0;
    }

    if (*self_).consumed_body == 0 {
        data = data.add((*self_).body_start as usize);
        len -= (*self_).body_start as usize;
        // PORT FIX: BodyStart is an offset into the read() buffer that
        // contained the end of the headers. In C, if no body bytes arrived in
        // that same read, the offset is applied again to the *next* buffer
        // (dropping bytes, or underflowing Len). Only apply it once.
        (*self_).body_start = 0;
    }

    if (*self_).body_cap < (*self_).content_length {
        (*self_).body = xrealloc((*self_).body as *mut c_void, (*self_).content_length as usize) as *mut c_char;
        (*self_).body_cap = (*self_).content_length;
    }

    // PORT FIX: the C version memcpy's `Len` bytes unconditionally, which
    // overflows `Body` when the client sends more than Content-Length bytes
    // (e.g. pipelined requests). Clamp to the remaining capacity.
    let remaining = ((*self_).content_length - (*self_).consumed_body) as usize;
    if len > remaining {
        len = remaining;
    }

    if !data.is_null() {
        ptr::copy_nonoverlapping(data, (*self_).body.add((*self_).consumed_body as usize), len);
    }

    (*self_).consumed_body += len as u32;

    if (*self_).consumed_body == (*self_).content_length {
        (*self_).state = HTTP_PARSER_STATE_COMPLETE;
    }

    0
}

pub unsafe fn http_parser_free(self_: *mut HttpParser) {
    for i in 0..HTTP_PARSER_HEADER_SIZE {
        xfree((*self_).headers[i].value as *mut c_void);
    }

    xfree((*self_).body as *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    struct Parser(Box<MaybeUninit<HttpParser>>);

    impl Parser {
        fn new() -> Parser {
            let mut p = Parser(Box::new(MaybeUninit::uninit()));
            unsafe { http_parser_init(p.ptr()) };
            p
        }
        fn ptr(&mut self) -> *mut HttpParser {
            self.0.as_mut_ptr()
        }
        fn get(&mut self) -> &HttpParser {
            unsafe { &*self.ptr() }
        }
        /// Same sequence as socket_actions_on_readable for one read().
        fn feed(&mut self, chunk: &[u8]) {
            let (len, data) = (chunk.len(), chunk.as_ptr() as *const c_char);
            unsafe {
                assert_eq!(http_parser_parse(self.ptr(), len, data), 0);
                let st = (*self.ptr()).state;
                if st == HTTP_PARSER_STATE_BODY || st == HTTP_PARSER_STATE_COMPLETE {
                    assert_eq!(http_parser_parse_body(self.ptr(), len, data), 0);
                }
            }
        }
        fn method(&mut self) -> String {
            let p = self.get();
            cstr(&p.method[..p.method_len as usize])
        }
        fn url(&mut self) -> String {
            let p = self.get();
            cstr(&p.url[..p.url_len as usize])
        }
        fn body(&mut self) -> Vec<u8> {
            let p = self.get();
            unsafe { std::slice::from_raw_parts(p.body as *const u8, p.content_length as usize).to_vec() }
        }
        fn header(&mut self, i: usize) -> (String, String) {
            let h = &self.get().headers[i];
            let v = unsafe { std::slice::from_raw_parts(h.value as *const u8, h.value_len as usize) };
            (cstr(&h.key[..h.key_len as usize]), String::from_utf8_lossy(v).into_owned())
        }
    }

    impl Drop for Parser {
        fn drop(&mut self) {
            unsafe { http_parser_free(self.ptr()) };
        }
    }

    fn cstr(s: &[c_char]) -> String {
        s.iter().map(|&c| c as u8 as char).collect()
    }

    const POST: &[u8] = b"POST /query HTTP/1.1\r\nHost: x\r\nContent-Length: 11\r\n\r\n{\"q\":\"abc\"}";

    #[test]
    fn get_without_body_completes_at_blank_line() {
        let mut p = Parser::new();
        p.feed(b"GET /health HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n\r\n");
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!((p.method(), p.url()), ("GET".into(), "/health".into()));
        assert_eq!(p.get().headers_len, 2);
        assert_eq!(p.header(0), ("Host".into(), "localhost".into()));
        assert_eq!(p.header(1), ("Accept".into(), "*/*".into()));
        assert_eq!(p.get().content_length, 0);
    }

    #[test]
    fn post_in_one_read() {
        let mut p = Parser::new();
        p.feed(POST);
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.url(), "/query");
        assert_eq!(p.body(), br#"{"q":"abc"}"#);
    }

    #[test]
    fn every_split_point_gives_the_same_request() {
        // Covers headers/body arriving in separate reads (PORT FIX 3).
        for split in 1..POST.len() {
            let mut p = Parser::new();
            p.feed(&POST[..split]);
            p.feed(&POST[split..]);
            assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE, "split at {split}");
            assert_eq!(p.body(), br#"{"q":"abc"}"#, "split at {split}");
        }
    }

    #[test]
    fn byte_by_byte() {
        let mut p = Parser::new();
        for b in POST {
            p.feed(std::slice::from_ref(b));
        }
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.body(), br#"{"q":"abc"}"#);
    }

    #[test]
    fn body_larger_than_initial_buffer() {
        let body = vec![b'x'; 50_000];
        let mut req = format!("POST /query HTTP/1.1\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
        req.extend_from_slice(&body);
        let mut p = Parser::new();
        for chunk in req.chunks(8192) {
            p.feed(chunk);
        }
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.body(), body);
        assert!(p.get().body_cap >= 50_000);
    }

    #[test]
    fn bytes_past_content_length_are_not_copied() {
        // PORT FIX 2: a pipelined second request must not overflow the body.
        let mut req = POST.to_vec();
        req.extend_from_slice(&[b'Z'; 4096]);
        let mut p = Parser::new();
        p.feed(&req);
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.get().consumed_body, 11);
        assert_eq!(p.body(), br#"{"q":"abc"}"#);
    }

    #[test]
    fn keep_alive_reuses_the_parser() {
        let mut p = Parser::new();
        p.feed(POST);
        p.feed(b"GET /health HTTP/1.1\r\n\r\n");
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!((p.method(), p.url()), ("GET".into(), "/health".into()));
        assert_eq!(p.get().headers_len, 0);
        assert_eq!(p.get().content_length, 0);
    }

    #[test]
    fn long_method_and_url_are_truncated_not_overflowed() {
        let url = format!("/{}", "u".repeat(200));
        let mut p = Parser::new();
        p.feed(format!("VERYLONGMETHOD {url} HTTP/1.1\r\n\r\n").as_bytes());
        assert_eq!(p.method(), "VERYLON"); // HttpParserMethodSize - 1
        assert_eq!(p.url().len(), HTTP_PARSER_URL_SIZE - 1);
        assert_eq!(p.get().url[HTTP_PARSER_URL_SIZE - 1], 0);
    }

    #[test]
    fn content_length_only_matches_two_spellings() {
        // Kept from C: only "Content-Length" and "content-length" are recognised.
        let mut p = Parser::new();
        p.feed(b"POST / HTTP/1.1\r\nCONTENT-LENGTH: 5\r\n\r\n");
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.get().content_length, 0);
    }

    #[test]
    fn twenty_four_headers_are_accepted() {
        let mut req = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..HTTP_PARSER_HEADER_SIZE {
            req.extend_from_slice(format!("h{i}: v{i}\r\n").as_bytes());
        }
        req.extend_from_slice(b"\r\n");
        let mut p = Parser::new();
        p.feed(&req);
        assert_eq!(p.get().state, HTTP_PARSER_STATE_COMPLETE);
        assert_eq!(p.get().headers_len as usize, HTTP_PARSER_HEADER_SIZE);
    }

    /// Known issue kept from C (out-of-bounds Headers[] write there; a bounds
    /// panic here). Flip this test when the parser is rewritten.
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn more_than_twenty_four_headers_panics() {
        let mut req = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..=HTTP_PARSER_HEADER_SIZE {
            req.extend_from_slice(format!("h{i}: v{i}\r\n").as_bytes());
        }
        req.extend_from_slice(b"\r\n");
        let mut p = Parser::new();
        p.feed(&req);
    }
}
