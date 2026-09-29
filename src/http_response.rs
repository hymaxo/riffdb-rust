pub const HTTP_RESPONSE_BUFFER_CAPACITY: usize = 4096;

const HTTP_PROTOCOL_STR: &[u8] = b"HTTP/1.1 ";

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

// room for headers, filled after the body
const HEAD_ROOM: usize = 64;

pub struct HttpResponse {
    pub buf: Vec<u8>,
    start: usize,
}

impl HttpResponse {
    pub fn new() -> HttpResponse {
        HttpResponse {
            buf: Vec::with_capacity(HTTP_RESPONSE_BUFFER_CAPACITY),
            start: 0,
        }
    }

    pub fn zero(&mut self) {
        self.buf.clear();
        self.start = 0;
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    pub fn begin_body_in_place(&mut self) -> &mut Vec<u8> {
        self.buf.clear();
        self.buf.resize(HEAD_ROOM, 0);
        self.start = HEAD_ROOM;
        &mut self.buf
    }

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

    pub fn status_code(&mut self, status: u16) {
        let mut digits = [0u8; 20];
        self.buf.extend_from_slice(HTTP_PROTOCOL_STR);
        self.buf.extend_from_slice(fmt_u64(status as u64, &mut digits));
        self.buf.extend_from_slice(b"\r\n");
    }

    pub fn body(&mut self, body: &[u8]) {
        let mut digits = [0u8; 20];
        let len_str = fmt_u64(body.len() as u64, &mut digits);

        self.buf.reserve(16 + len_str.len() + 4 + body.len());
        self.buf.extend_from_slice(b"Content-Length: ");
        self.buf.extend_from_slice(len_str);
        self.buf.extend_from_slice(b"\r\n\r\n");
        self.buf.extend_from_slice(body);
    }

    pub fn status_and_body(&mut self, status: u16, body: &[u8]) {
        self.status_code(status);
        self.body(body);
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
    fn wire_format() {
        let mut r = HttpResponse::new();

        r.status_code(200);
        r.body(b"health");
        assert_eq!(r.buf, b"HTTP/1.1 200\r\nContent-Length: 6\r\n\r\nhealth");

        r.zero();
        r.status_code(404);
        r.body(b"");
        assert_eq!(r.buf, b"HTTP/1.1 404\r\nContent-Length: 0\r\n\r\n");

        r.zero();
        let big = vec![b'x'; 100_000];
        r.status_code(200);
        r.body(&big);
        let head = b"HTTP/1.1 200\r\nContent-Length: 100000\r\n\r\n";
        assert_eq!(&r.buf[..head.len()], head);
        assert_eq!(r.buf.len(), head.len() + 100_000);
    }

    #[test]
    fn in_place_body_gives_the_same_bytes() {
        let big = vec![b'x'; 100_000];
        for (status, body) in [(200u16, &b"[]"[..]), (200, &big[..]), (65535, &b""[..]), (500, &b"err"[..])] {
            let mut normal = HttpResponse::new();
            normal.status_code(status);
            normal.body(body);

            let mut in_place = HttpResponse::new();
            in_place.status_code(404);
            in_place.begin_body_in_place().extend_from_slice(body);
            in_place.finish_body_in_place(status);

            assert_eq!(in_place.bytes(), normal.bytes());
        }
    }
}
