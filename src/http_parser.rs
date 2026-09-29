// Incremental HTTP/1.1 request parser: a byte-at-a-time state machine that
// can be fed whatever each read() returns.
//
// Method, URL and header keys go into fixed buffers and are truncated when
// too long. Header values grow on demand up to 8191 bytes. Headers past the
// 24th are ignored. The HTTP version is not checked.

pub const HTTP_PARSER_METHOD_SIZE: usize = 8;
pub const HTTP_PARSER_URL_SIZE: usize = 64;
pub const HTTP_PARSER_HEADER_SIZE: usize = 24;
pub const HTTP_PARSER_HEADER_KEY_SIZE: usize = 64;
pub const HTTP_PARSER_HEADER_VALUE_SIZE: usize = 8192;
pub const HTTP_PARSER_BODY_SIZE: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParserState {
    Method,
    Url,
    Version,
    HeaderKey,
    HeaderValue,
    Body,
    Complete,
}

pub struct HttpHeader {
    pub key: [u8; HTTP_PARSER_HEADER_KEY_SIZE],
    pub key_len: u8,
    pub value: Vec<u8>,
}

impl HttpHeader {
    fn new() -> HttpHeader {
        HttpHeader {
            key: [0; HTTP_PARSER_HEADER_KEY_SIZE],
            key_len: 0,
            value: Vec::new(),
        }
    }

    pub fn key(&self) -> &[u8] {
        &self.key[..self.key_len as usize]
    }
}

pub struct HttpParser {
    pub state: ParserState,
    pub saw_cr: bool,
    pub saw_double_dot: bool,

    pub method: [u8; HTTP_PARSER_METHOD_SIZE],
    pub method_len: u8,

    pub url: [u8; HTTP_PARSER_URL_SIZE],
    pub url_len: u8,

    pub headers_len: u8,
    pub headers: [HttpHeader; HTTP_PARSER_HEADER_SIZE],

    pub body: Vec<u8>,
    pub body_start: u32,
    pub content_length: u32,
}

/// Parses Content-Length the way `strtoul(s, NULL, 10)` would, cast to u32:
/// leading whitespace and a sign are allowed (a minus negates in `unsigned
/// long`), parsing stops at the first non-digit, and overflow saturates at
/// ULONG_MAX. Existing clients rely on this being lenient.
fn strtoul_u32(s: &[u8]) -> u32 {
    // unsigned long is 32-bit on Windows, 64-bit on LP64 targets.
    #[cfg(windows)]
    type ULong = u32;
    #[cfg(not(windows))]
    type ULong = u64;

    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
        i += 1;
    }
    let mut negative = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        negative = s[i] == b'-';
        i += 1;
    }
    let mut v: ULong = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        match v.checked_mul(10).and_then(|v| v.checked_add((s[i] - b'0') as ULong)) {
            Some(n) => v = n,
            None => overflow = true,
        }
        i += 1;
    }
    let v = if overflow {
        ULong::MAX
    } else if negative {
        v.wrapping_neg()
    } else {
        v
    };
    v as u32
}

impl HttpParser {
    pub fn new() -> HttpParser {
        HttpParser {
            state: ParserState::Method,
            saw_cr: false,
            saw_double_dot: false,
            method: [0; HTTP_PARSER_METHOD_SIZE],
            method_len: 0,
            url: [0; HTTP_PARSER_URL_SIZE],
            url_len: 0,
            headers_len: 0,
            headers: std::array::from_fn(|_| HttpHeader::new()),
            body: Vec::with_capacity(HTTP_PARSER_BODY_SIZE),
            body_start: 0,
            content_length: 0,
        }
    }

    pub fn method(&self) -> &[u8] {
        &self.method[..self.method_len as usize]
    }

    pub fn url(&self) -> &[u8] {
        &self.url[..self.url_len as usize]
    }

    /// Looks up Content-Length. Only the exact spellings `Content-Length`
    /// and `content-length` are recognised.
    fn set_content_length(&mut self) -> u32 {
        for h in &self.headers[..self.headers_len as usize] {
            if h.key_len == 14 && (h.key() == b"Content-Length" || h.key() == b"content-length") {
                self.content_length = strtoul_u32(&h.value);
                return self.content_length;
            }
        }

        0
    }

    /// Resets the parser for the next request on the connection.
    pub fn zero(&mut self) {
        self.state = ParserState::Method;
        self.saw_cr = false;
        self.saw_double_dot = false;
        self.method_len = 0;
        self.url_len = 0;
        self.content_length = 0;
        self.body.clear();

        for h in &mut self.headers[..self.headers_len as usize] {
            h.key_len = 0;
            h.value.clear();
        }

        self.headers_len = 0;
    }

    /// Feeds the request line and headers. Stops at the end of the headers;
    /// the body is fed separately with `parse_body`.
    pub fn parse(&mut self, data: &[u8]) {
        if self.state == ParserState::Body {
            return;
        }

        if self.state == ParserState::Complete {
            self.zero();
        }

        for (i, &byte) in data.iter().enumerate() {
            match self.state {
                ParserState::Method => {
                    if byte == b' ' {
                        self.method[self.method_len as usize] = 0;
                        self.state = ParserState::Url;
                        continue;
                    }

                    if self.method_len as usize >= HTTP_PARSER_METHOD_SIZE - 1 {
                        continue;
                    }

                    self.method[self.method_len as usize] = byte;
                    self.method_len += 1;
                }
                ParserState::Url => {
                    if byte == b' ' {
                        self.url[self.url_len as usize] = 0;
                        self.state = ParserState::Version;
                        continue;
                    }

                    if self.url_len as usize >= HTTP_PARSER_URL_SIZE - 1 {
                        continue;
                    }

                    self.url[self.url_len as usize] = byte;
                    self.url_len += 1;
                }
                ParserState::Version => {
                    if byte == b'\r' {
                        self.saw_cr = true;
                        continue;
                    }

                    if byte == b'\n' && self.saw_cr {
                        self.state = ParserState::HeaderKey;
                        self.saw_cr = false;
                        continue;
                    }

                    // the version itself is ignored
                }
                ParserState::HeaderKey => {
                    if byte == b':' {
                        self.saw_double_dot = true;
                        continue;
                    }

                    if byte == b' ' && self.saw_double_dot {
                        // Past the last slot the header is dropped.
                        if let Some(h) = self.headers.get_mut(self.headers_len as usize) {
                            h.key[h.key_len as usize] = 0;
                        }
                        self.state = ParserState::HeaderValue;
                        self.saw_double_dot = false;
                        continue;
                    }

                    if byte == b'\r' {
                        self.saw_cr = true;
                        continue;
                    }

                    if byte == b'\n' && self.saw_cr {
                        self.saw_cr = false;

                        self.set_content_length();
                        if self.content_length == 0 {
                            self.state = ParserState::Complete;
                            return;
                        }

                        self.state = ParserState::Body;
                        self.body_start = (i + 1) as u32;
                        return;
                    }

                    if self.headers_len as usize >= HTTP_PARSER_HEADER_SIZE - 1 {
                        continue;
                    }

                    let h = &mut self.headers[self.headers_len as usize];
                    if h.key_len as usize >= HTTP_PARSER_HEADER_KEY_SIZE - 1 {
                        continue;
                    }

                    h.key[h.key_len as usize] = byte;
                    h.key_len += 1;
                }
                ParserState::HeaderValue => {
                    if byte == b'\r' {
                        self.saw_cr = true;
                        continue;
                    }

                    if byte == b'\n' && self.saw_cr {
                        self.state = ParserState::HeaderKey;
                        self.saw_cr = false;
                        if (self.headers_len as usize) < HTTP_PARSER_HEADER_SIZE {
                            self.headers_len += 1;
                        }
                        continue;
                    }

                    let Some(h) = self.headers.get_mut(self.headers_len as usize) else {
                        continue;
                    };
                    if h.value.len() >= HTTP_PARSER_HEADER_VALUE_SIZE - 1 {
                        continue;
                    }

                    h.value.push(byte);
                }
                ParserState::Body | ParserState::Complete => return,
            }
        }
    }

    /// Feeds body bytes from the same buffer that was passed to `parse`.
    pub fn parse_body(&mut self, mut data: &[u8]) {
        if self.state != ParserState::Body {
            return;
        }

        if self.body.is_empty() {
            data = &data[self.body_start as usize..];
            // body_start is an offset into the buffer that held the end of
            // the headers. It must only apply to that buffer, not to the next
            // one when the body arrives in a later read.
            self.body_start = 0;
        }

        let content_length = self.content_length as usize;
        if self.body.capacity() < content_length {
            self.body.reserve_exact(content_length - self.body.len());
        }

        // Anything past Content-Length (a pipelined request) is not ours.
        let remaining = content_length - self.body.len();
        self.body.extend_from_slice(&data[..data.len().min(remaining)]);

        if self.body.len() == content_length {
            self.state = ParserState::Complete;
        }
    }

    /// Hands the completed body over (moves it; no copy). The next body is
    /// allocated when it arrives.
    pub fn take_body(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.body)
    }
}

impl Default for HttpParser {
    fn default() -> Self {
        HttpParser::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same sequence as `on_readable` for one read().
    fn feed(p: &mut HttpParser, chunk: &[u8]) {
        p.parse(chunk);
        if p.state == ParserState::Body || p.state == ParserState::Complete {
            p.parse_body(chunk);
        }
    }

    fn s(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    const POST: &[u8] = b"POST /query HTTP/1.1\r\nHost: x\r\nContent-Length: 11\r\n\r\n{\"q\":\"abc\"}";

    #[test]
    fn get_without_body_completes_at_blank_line() {
        let mut p = HttpParser::new();
        feed(&mut p, b"GET /health HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n\r\n");
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!((s(p.method()), s(p.url())), ("GET".into(), "/health".into()));
        assert_eq!(p.headers_len, 2);
        assert_eq!((s(p.headers[0].key()), s(&p.headers[0].value)), ("Host".into(), "localhost".into()));
        assert_eq!((s(p.headers[1].key()), s(&p.headers[1].value)), ("Accept".into(), "*/*".into()));
        assert_eq!(p.content_length, 0);
    }

    #[test]
    fn post_in_one_read() {
        let mut p = HttpParser::new();
        feed(&mut p, POST);
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.url(), b"/query");
        assert_eq!(p.body, br#"{"q":"abc"}"#);
    }

    #[test]
    fn every_split_point_gives_the_same_request() {
        // Covers headers and body arriving in separate reads.
        for split in 1..POST.len() {
            let mut p = HttpParser::new();
            feed(&mut p, &POST[..split]);
            feed(&mut p, &POST[split..]);
            assert_eq!(p.state, ParserState::Complete, "split at {split}");
            assert_eq!(p.body, br#"{"q":"abc"}"#, "split at {split}");
        }
    }

    #[test]
    fn byte_by_byte() {
        let mut p = HttpParser::new();
        for b in POST {
            feed(&mut p, std::slice::from_ref(b));
        }
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.body, br#"{"q":"abc"}"#);
    }

    #[test]
    fn body_larger_than_initial_buffer() {
        let body = vec![b'x'; 50_000];
        let mut req = format!("POST /query HTTP/1.1\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
        req.extend_from_slice(&body);
        let mut p = HttpParser::new();
        for chunk in req.chunks(8192) {
            feed(&mut p, chunk);
        }
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.body, body);
    }

    #[test]
    fn bytes_past_content_length_are_not_copied() {
        // A pipelined second request must not end up in the body.
        let mut req = POST.to_vec();
        req.extend_from_slice(&[b'Z'; 4096]);
        let mut p = HttpParser::new();
        feed(&mut p, &req);
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.body, br#"{"q":"abc"}"#);
    }

    #[test]
    fn keep_alive_reuses_the_parser() {
        let mut p = HttpParser::new();
        feed(&mut p, POST);
        assert_eq!(p.take_body(), br#"{"q":"abc"}"#);
        feed(&mut p, b"GET /health HTTP/1.1\r\n\r\n");
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!((s(p.method()), s(p.url())), ("GET".into(), "/health".into()));
        assert_eq!(p.headers_len, 0);
        assert_eq!(p.content_length, 0);
        feed(&mut p, POST);
        assert_eq!(p.body, br#"{"q":"abc"}"#);
    }

    #[test]
    fn long_method_and_url_are_truncated_not_overflowed() {
        let url = format!("/{}", "u".repeat(200));
        let mut p = HttpParser::new();
        feed(&mut p, format!("VERYLONGMETHOD {url} HTTP/1.1\r\n\r\n").as_bytes());
        assert_eq!(p.method(), b"VERYLON"); // HTTP_PARSER_METHOD_SIZE - 1
        assert_eq!(p.url().len(), HTTP_PARSER_URL_SIZE - 1);
        assert_eq!(p.url[HTTP_PARSER_URL_SIZE - 1], 0);
    }

    #[test]
    fn content_length_only_matches_two_spellings() {
        let mut p = HttpParser::new();
        feed(&mut p, b"POST / HTTP/1.1\r\nCONTENT-LENGTH: 5\r\n\r\n");
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.content_length, 0);
    }

    #[test]
    fn strtoul_quirks() {
        assert_eq!(strtoul_u32(b"42"), 42);
        assert_eq!(strtoul_u32(b"  \t+42abc"), 42);
        assert_eq!(strtoul_u32(b"abc"), 0);
        assert_eq!(strtoul_u32(b""), 0);
        assert_eq!(strtoul_u32(b"-1"), u32::MAX); // -(1) in unsigned long
        assert_eq!(strtoul_u32(b"4294967296"), if cfg!(windows) { u32::MAX } else { 0 });
        assert_eq!(strtoul_u32(b"99999999999999999999999"), u32::MAX);
    }

    #[test]
    fn twenty_four_headers_are_accepted() {
        let mut req = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..HTTP_PARSER_HEADER_SIZE {
            req.extend_from_slice(format!("h{i}: v{i}\r\n").as_bytes());
        }
        req.extend_from_slice(b"\r\n");
        let mut p = HttpParser::new();
        feed(&mut p, &req);
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.headers_len as usize, HTTP_PARSER_HEADER_SIZE);
    }

    #[test]
    fn more_than_twenty_four_headers_are_ignored() {
        // The extra headers are dropped, and a Content-Length among the first
        // 24 still works.
        let mut req = b"POST / HTTP/1.1\r\nContent-Length: 2\r\n".to_vec();
        for i in 0..40 {
            req.extend_from_slice(format!("h{i}: v{i}\r\n").as_bytes());
        }
        req.extend_from_slice(b"\r\nok");
        let mut p = HttpParser::new();
        feed(&mut p, &req);
        assert_eq!(p.state, ParserState::Complete);
        assert_eq!(p.headers_len as usize, HTTP_PARSER_HEADER_SIZE);
        assert_eq!(p.body, b"ok");
    }
}
