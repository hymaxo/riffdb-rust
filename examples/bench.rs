// Tiny keep-alive load generator for riffdb.
//
//   cargo run --release --example bench -- <port> [connections] [seconds] [scenarios]
//
// Runs each scenario against an already running server and prints req/s and
// latency percentiles. One OS thread per connection, one request in flight per
// connection (like the JS client's fetch keep-alive pool).
//
// `scenarios` is an optional comma-separated filter (e.g. `health,execute`).
// Failures (I/O errors, timeouts, non-200, malformed responses) are counted
// and the connection is reopened, so a misbehaving server is measured rather
// than aborting the run. `compare/` also uses it against the C implementation.

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

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

/// Sends one request and reads exactly one response. Returns the status and
/// the body length.
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
            let head = std::str::from_utf8(&buf[..pos]).map_err(|_| bad("non-utf8 head"))?;
            let status = head
                .lines()
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|s| s.trim().parse().ok())
                .ok_or_else(|| bad("bad status line"))?;
            let cl = match head.lines().find_map(|l| l.strip_prefix("Content-Length: ")) {
                Some(v) => v.trim().parse::<usize>().map_err(|_| bad("bad content-length"))?,
                None => 0,
            };
            break (pos + 4, cl, status);
        }
        if buf.len() > 1 << 16 {
            return Err(bad("no header end"));
        }
    };
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    // Keep-alive framing: exactly one response, nothing trailing.
    if buf.len() != header_end + content_length {
        return Err(bad("trailing bytes"));
    }
    Ok((status, content_length))
}

fn connect(port: u16) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

struct ThreadResult {
    ok: u64,
    errors: u64,
    first_error: Option<String>,
    lat_us: Vec<u32>,
    body_len: usize,
}

fn run(port: u16, conns: usize, secs: u64, sc: &Scenario) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let results: Vec<ThreadResult> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..conns)
            .map(|_| {
                s.spawn(|| {
                    let mut r = ThreadResult { ok: 0, errors: 0, first_error: None, lat_us: Vec::with_capacity(1 << 16), body_len: 0 };
                    let mut buf = Vec::with_capacity(1 << 20);
                    let mut stream = None;
                    while Instant::now() < deadline {
                        let s = match stream.as_mut() {
                            Some(s) => s,
                            None => match connect(port) {
                                Ok(s) => stream.insert(s),
                                Err(e) => {
                                    r.errors += 1;
                                    r.first_error.get_or_insert_with(|| format!("connect: {e}"));
                                    std::thread::sleep(Duration::from_millis(50));
                                    continue;
                                }
                            },
                        };
                        let t = Instant::now();
                        match round_trip(s, &sc.request, &mut buf) {
                            Ok((200, len)) => {
                                r.lat_us.push(t.elapsed().as_micros() as u32);
                                r.body_len = len;
                                r.ok += 1;
                            }
                            Ok((status, _)) => {
                                r.errors += 1;
                                r.first_error.get_or_insert_with(|| {
                                    format!("status {status}: {}", String::from_utf8_lossy(&buf[..buf.len().min(200)]))
                                });
                                stream = None;
                            }
                            Err(e) => {
                                r.errors += 1;
                                r.first_error.get_or_insert_with(|| format!("{e}"));
                                stream = None;
                            }
                        }
                    }
                    r
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let total: u64 = results.iter().map(|r| r.ok).sum();
    let errors: u64 = results.iter().map(|r| r.errors).sum();
    let mut lat: Vec<u32> = results.iter().flat_map(|r| r.lat_us.iter().copied()).collect();
    lat.sort_unstable();
    let pct = |p: f64| if lat.is_empty() { 0 } else { lat[((lat.len() as f64 * p) as usize).min(lat.len() - 1)] };
    println!(
        "{:<14} {:>10.0} req/s   p50 {:>6}us   p99 {:>6}us   body {:>7} B   errors {}",
        sc.name,
        total as f64 / secs as f64,
        pct(0.50),
        pct(0.99),
        results.iter().map(|r| r.body_len).max().unwrap_or(0),
        errors
    );
    if let Some(e) = results.iter().find_map(|r| r.first_error.as_ref()) {
        println!("{:<14} first error: {}", "", e.replace(['\r', '\n'], " "));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args.get(1).expect("usage: bench <port> [conns] [secs] [scenarios]").parse().unwrap();
    let conns: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(8);
    let secs: u64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(5);
    let only: Option<Vec<&str>> = args.get(4).map(|s| s.split(',').collect());

    // Setup: a table with 1000 rows (mixed types, some strings needing escapes).
    let mut setup = connect(port).expect("server not reachable");
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
        // ~4 KB of JSON: the largest result the C implementation can return
        // (its response buffer grows once, 4 -> 8 KiB, then overflows).
        Scenario {
            name: "query_50",
            request: post("/query", r#"{"q":"SELECT * FROM bench LIMIT 50"}"#),
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
        if only.as_ref().is_none_or(|o| o.contains(&sc.name)) {
            run(port, conns, secs, sc);
        }
    }
}
