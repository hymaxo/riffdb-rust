// Tiny keep-alive load generator for riffdb.
//
//   cargo run --release --example bench -- <port> [connections] [seconds]
//
// Runs each scenario against an already running server and prints req/s and
// latency percentiles. One OS thread per connection, one request in flight per
// connection (like the JS client's fetch keep-alive pool).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

struct Scenario {
    name: &'static str,
    request: Vec<u8>,
}

fn post(path: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: bench\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Sends one request and reads exactly one response. Returns the body length.
fn round_trip(stream: &mut TcpStream, req: &[u8], buf: &mut Vec<u8>) -> std::io::Result<(u16, usize)> {
    stream.write_all(req)?;
    buf.clear();
    let mut chunk = [0u8; 65536];
    let (header_end, content_length, status) = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(&buf[..pos]).unwrap();
            let status = head.lines().next().unwrap().split(' ').nth(1).unwrap().trim().parse().unwrap();
            let cl = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .map(|v| v.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            break (pos + 4, cl, status);
        }
    };
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok((status, content_length))
}

fn run(port: u16, conns: usize, secs: u64, sc: &Scenario) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let results: Vec<(u64, Vec<u32>, usize)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..conns)
            .map(|_| {
                s.spawn(|| {
                    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                    stream.set_nodelay(true).unwrap();
                    let mut buf = Vec::with_capacity(1 << 20);
                    let mut lat_us = Vec::with_capacity(1 << 16);
                    let mut n = 0u64;
                    let mut body_len = 0;
                    while Instant::now() < deadline {
                        let t = Instant::now();
                        let (status, len) = round_trip(&mut stream, &sc.request, &mut buf).unwrap();
                        assert_eq!(status, 200, "{}: {}", sc.name, String::from_utf8_lossy(&buf));
                        lat_us.push(t.elapsed().as_micros() as u32);
                        body_len = len;
                        n += 1;
                    }
                    (n, lat_us, body_len)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let total: u64 = results.iter().map(|r| r.0).sum();
    let mut lat: Vec<u32> = results.iter().flat_map(|r| r.1.iter().copied()).collect();
    lat.sort_unstable();
    let pct = |p: f64| lat[((lat.len() as f64 * p) as usize).min(lat.len() - 1)];
    println!(
        "{:<14} {:>10.0} req/s   p50 {:>6}us   p99 {:>6}us   body {:>7} B",
        sc.name,
        total as f64 / secs as f64,
        pct(0.50),
        pct(0.99),
        results[0].2
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args.get(1).expect("usage: bench <port> [conns] [secs]").parse().unwrap();
    let conns: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(8);
    let secs: u64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(5);

    // Setup: a table with 1000 rows (mixed types, some strings needing escapes).
    let mut setup = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut buf = Vec::new();
    for q in [
        r#"{"q":"DROP TABLE IF EXISTS bench"}"#.to_string(),
        r#"{"q":"CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, score REAL, note TEXT)"}"#.to_string(),
        r#"{"q":"WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<1000) INSERT INTO bench SELECT x, 'user ' || x, x * 1.5, 'line1\nline2 \"quoted\" ' || hex(randomblob(8)) FROM c"}"#.to_string(),
    ] {
        let (status, _) = round_trip(&mut setup, &post("/execute", &q), &mut buf).unwrap();
        assert_eq!(status, 200, "setup failed: {}", String::from_utf8_lossy(&buf));
    }
    drop(setup);

    let scenarios = [
        Scenario {
            name: "health",
            request: b"GET /health HTTP/1.1\r\nHost: bench\r\n\r\n".to_vec(),
        },
        Scenario {
            name: "query_point",
            request: post("/query", r#"{"q":"SELECT id, name, score FROM bench WHERE id = ?","args":[500]}"#),
        },
        Scenario {
            name: "query_1000",
            request: post("/query", r#"{"q":"SELECT * FROM bench"}"#),
        },
        Scenario {
            name: "execute",
            request: post("/execute", r#"{"q":"UPDATE bench SET score = score WHERE id = ?","args":[1]}"#),
        },
    ];

    println!("{} connections, {}s per scenario", conns, secs);
    for sc in &scenarios {
        run(port, conns, secs, sc);
    }
}
