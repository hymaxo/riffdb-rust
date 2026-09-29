// Port of HttpResponse.h / HttpResponse.c

pub const HTTP_RESPONSE_BUFFER_CAPACITY: usize = 4096;

const HTTP_PROTOCOL_STR: &[u8] = b"HTTP/1.1 ";

/// Formats `v` in decimal into the tail of `buf` and returns the digits
/// (what C does with sprintf("%d") into a stack buffer).
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

/// Room left in front of an in-place body for the status line and header:
/// "HTTP/1.1 65535\r\nContent-Length: 18446744073709551615\r\n\r\n" is 56.
const HEAD_ROOM: usize = 64;

pub struct HttpResponse {
    pub buf: Vec<u8>,
    /// Where the response starts in `buf` (non-zero after an in-place body).
    start: usize,
}

impl HttpResponse {
    /// HttpResponseInit
    pub fn new() -> HttpResponse {
        HttpResponse {
            buf: Vec::with_capacity(HTTP_RESPONSE_BUFFER_CAPACITY),
            start: 0,
        }
    }

    /// HttpResponseZero
    pub fn zero(&mut self) {
        self.buf.clear();
        self.start = 0;
    }

    /// The bytes to send.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    /// Starts a body that is written straight into `buf` (append to the
    /// returned Vec), for when its length isn't known up front. Finish with
    /// `finish_body_in_place`, or `zero()` to abandon it.
    ///
    /// Not in C: /query built its JSON in a separate buffer and copied it
    /// into the response (yyjson output -> HttpResponseBody).
    pub fn begin_body_in_place(&mut self) -> &mut Vec<u8> {
        self.buf.clear();
        self.buf.resize(HEAD_ROOM, 0);
        self.start = HEAD_ROOM;
        &mut self.buf
    }

    /// Writes status line + Content-Length right-aligned in front of the
    /// in-place body. Same bytes as status_code() followed by body().
    pub fn finish_body_in_place(&mut self, status: u16) {
        let body_len = self.buf.len() - HEAD_ROOM;
        let mut head = [0u8; HEAD_ROOM];
        let mut n = 0;
        let mut put = |s: &[u8]| {
            head[n..n + s.len()].copy_from_slice(s);
            n += s.len();
        };
        let mut digits = [0u8; 20];
        put(HTTP_PROTOCOL_STR);
        put(fmt_u64(status as u64, &mut digits));
        put(b"\r\nContent-Length: ");
        put(fmt_u64(body_len as u64, &mut digits));
        put(b"\r\n\r\n");

        self.start = HEAD_ROOM - n;
        self.buf[self.start..HEAD_ROOM].copy_from_slice(&head[..n]);
    }

    /// HttpResponseStatusCode
    pub fn status_code(&mut self, status: u16) {
        let mut digits = [0u8; 20];
        self.buf.extend_from_slice(HTTP_PROTOCOL_STR);
        self.buf.extend_from_slice(fmt_u64(status as u64, &mut digits));
        self.buf.extend_from_slice(b"\r\n");
    }

    /// HttpResponseHeader
    #[allow(dead_code)] // C API; body() writes its one header inline
    pub fn header(&mut self, key: &[u8], value: &[u8]) {
        self.buf.extend_from_slice(key);
        self.buf.extend_from_slice(b": ");
        self.buf.extend_from_slice(value);
        self.buf.extend_from_slice(b"\r\n");
    }

    /// HttpResponseBody
    pub fn body(&mut self, body: &[u8]) {
        let mut digits = [0u8; 20];
        let len_str = fmt_u64(body.len() as u64, &mut digits);

        // Grow once for the rest of the response instead of per append
        // (C: HttpResponseAppend doubles per call).
        self.buf.reserve(16 + len_str.len() + 4 + body.len());
        self.buf.extend_from_slice(b"Content-Length: ");
        self.buf.extend_from_slice(len_str);
        self.buf.extend_from_slice(b"\r\n\r\n");
        self.buf.extend_from_slice(body);
    }
}

impl Default for HttpResponse {
    fn default() -> Self {
        HttpResponse::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_u64_matches_to_string() {
        let mut buf = [0u8; 20];
        for v in [0u64, 1, 9, 10, 200, 404, 65535, 4294967295, u64::MAX] {
            assert_eq!(fmt_u64(v, &mut buf), v.to_string().as_bytes());
        }
    }

    #[test]
    fn wire_format_matches_c() {
        let mut r = HttpResponse::new();

        r.status_code(200);
        r.body(b"health");
        assert_eq!(r.buf, b"HTTP/1.1 200\r\nContent-Length: 6\r\n\r\nhealth");

        r.zero();
        r.status_code(404);
        r.body(b"");
        assert_eq!(r.buf, b"HTTP/1.1 404\r\nContent-Length: 0\r\n\r\n");

        // Larger than two doublings of the initial capacity (PORT FIX 1).
        r.zero();
        let big = vec![b'x'; 100_000];
        r.status_code(200);
        r.body(&big);
        let head = b"HTTP/1.1 200\r\nContent-Length: 100000\r\n\r\n";
        assert_eq!(&r.buf[..head.len()], head);
        assert_eq!(r.buf.len(), head.len() + 100_000);

        r.zero();
        r.header(b"X-Test", b"1");
        assert_eq!(r.buf, b"X-Test: 1\r\n");
    }

    #[test]
    fn in_place_body_gives_the_same_bytes() {
        let big = vec![b'x'; 100_000];
        for (status, body) in [(200u16, &b"[]"[..]), (200, &big[..]), (65535, &b""[..]), (500, &b"err"[..])] {
            let mut normal = HttpResponse::new();
            normal.status_code(status);
            normal.body(body);

            let mut in_place = HttpResponse::new();
            in_place.status_code(404); // leftovers must not leak into the result
            in_place.begin_body_in_place().extend_from_slice(body);
            in_place.finish_body_in_place(status);

            assert_eq!(in_place.bytes(), normal.bytes());
        }
    }
}
