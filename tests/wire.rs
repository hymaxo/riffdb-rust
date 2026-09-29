// Wire-level regression tests: exact bytes on a raw keep-alive socket,
// against a multi-worker server.

mod common;

use common::Server;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn connect(s: &Server) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
}

/// Reads exactly one response off a keep-alive connection: (status, body).
fn read_response(stream: &mut TcpStream) -> (u16, Vec<u8>) {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).unwrap(), 1, "eof in headers: {:?}", String::from_utf8_lossy(&buf));
        buf.push(byte[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap();
    assert!(status_line.starts_with("HTTP/1.1 "), "bad status line {status_line:?} in {head:?}");
    let status = status_line[9..].parse().unwrap();
    let len: usize = lines
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .unwrap_or_else(|| panic!("no Content-Length in {head:?}"))
        .parse()
        .unwrap();
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).unwrap();
    (status, body)
}

fn post(path: &str, body: &str) -> Vec<u8> {
    format!("POST {path} HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

#[test]
fn exact_response_bytes() {
    let s = Server::start();
    let mut c = connect(&s);
    c.write_all(b"GET /health HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    let mut buf = vec![0u8; 256];
    let expected = b"HTTP/1.1 200\r\nContent-Length: 6\r\n\r\nhealth";
    let mut got = Vec::new();
    while got.len() < expected.len() {
        let n = c.read(&mut buf).unwrap();
        assert!(n > 0);
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, expected);
}

#[test]
fn keep_alive_with_many_workers_never_mixes_responses() {
    // Back-to-back requests on one connection land on different workers,
    // and each response must still arrive whole and in order.
    let s = Server::start_with_threads(4);
    let mut c = connect(&s);
    for i in 0..2000 {
        let (req, want): (Vec<u8>, Vec<u8>) = match i % 3 {
            0 => (b"GET /health HTTP/1.1\r\n\r\n".to_vec(), b"health".to_vec()),
            1 => (b"GET /nope HTTP/1.1\r\n\r\n".to_vec(), b"not found".to_vec()),
            _ => (
                post("/query", &format!(r#"{{"q":"SELECT ? AS i","args":[{i}]}}"#)),
                format!(r#"[{{"i":{i}}}]"#).into_bytes(),
            ),
        };
        c.write_all(&req).unwrap();
        let (_, body) = read_response(&mut c);
        assert_eq!(String::from_utf8_lossy(&body), String::from_utf8_lossy(&want), "request #{i}");
    }
}

#[test]
fn headers_and_body_in_separate_writes() {
    // The body arrives in later read()s than the headers.
    let s = Server::start_with_threads(4);
    let mut c = connect(&s);
    for _ in 0..20 {
        let body = r#"{"q":"SELECT 'split' AS s"}"#;
        c.write_all(format!("POST /query HTTP/1.1\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        c.write_all(&body.as_bytes()[..10]).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        c.write_all(&body.as_bytes()[10..]).unwrap();
        let (status, got) = read_response(&mut c);
        assert_eq!((status, got), (200, br#"[{"s":"split"}]"#.to_vec()));
    }
}

#[test]
fn large_request_body_and_large_response() {
    // Responses far past the initial buffer size, and bodies spread over
    // many reads.
    let s = Server::start_with_threads(4);
    let mut c = connect(&s);
    let big = "x".repeat(200_000);
    c.write_all(&post("/query", &format!(r#"{{"q":"SELECT length(?) AS n, ? AS s","args":["{big}","{big}"]}}"#)))
        .unwrap();
    let (status, body) = read_response(&mut c);
    assert_eq!(status, 200);
    assert_eq!(body, format!(r#"[{{"n":200000,"s":"{big}"}}]"#).into_bytes());

    // and the connection is still usable afterwards
    c.write_all(b"GET /health HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut c), (200, b"health".to_vec()));
}

#[test]
fn error_responses() {
    let s = Server::start_with_threads(2);
    let mut c = connect(&s);
    for (req, want) in [
        (post("/query", r#"{"q":"SELEC 1"}"#), (500, r#"near "SELEC": syntax error"#)),
        (post("/query", "not json"), (500, "query len < 3")),
        (post("/query", r#"{"x":1}"#), (500, "query is empty")),
        (post("/query", r#"{"q":"ab"}"#), (500, "query len < 3")),
        (post("/execute", r#"{"q":"SELECT * FROM missing"}"#), (500, "no such table: missing")),
    ] {
        c.write_all(&req).unwrap();
        let (status, body) = read_response(&mut c);
        assert_eq!((status, String::from_utf8(body).unwrap().as_str()), want);
    }
}
