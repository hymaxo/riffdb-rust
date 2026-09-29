// Test harness: spawns the real riffdb binary and talks to it over HTTP the
// same way packages/riffdb.js/connection.ts does.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

/// A riffdb server process with its own data directory, killed on drop.
pub struct Server {
    child: Child,
    dir: PathBuf,
    pub port: u16,
}

impl Server {
    /// One worker thread, like `.vscode/launch.json` (`-t 1`). Per-connection
    /// state such as `PRAGMA foreign_keys` only sticks with a single worker,
    /// since requests are dispatched round-robin across worker connections.
    pub fn start() -> Server {
        Server::start_with_threads(1)
    }

    pub fn start_with_threads(threads: u8) -> Server {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("riffdb-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);

        // RIFFDB_BIN lets the same suite run against another build (an older
        // version, a profiling build, or the C server).
        let bin = std::env::var_os("RIFFDB_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_riffdb").into());

        // If the port turns out to be taken, the server exits at startup
        // (bind fails): try the next one.
        for _ in 0..20 {
            let port = next_port();
            let child = Command::new(&bin)
                .arg("-p")
                .arg(port.to_string())
                .arg("-t")
                .arg(threads.to_string())
                .arg("-d")
                .arg(&dir)
                .env("NO_COLOR", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("failed to spawn riffdb");

            let mut server = Server { child, dir: dir.clone(), port };
            if server.wait_ready() {
                return server;
            }
        }
        panic!("could not start riffdb");
    }

    /// true once /health answers; false if the process exited first.
    fn wait_ready(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return false;
            }
            if let Ok((200, body)) = http(self.port, "GET", "/health", "") {
                if body == "health" {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("riffdb did not become ready on port {}", self.port);
    }

    pub fn db(&self) -> Db {
        Db { port: self.port }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Ports handed out by this test process never repeat, so two servers of
/// one run can't end up on the same port. (Asking the OS for a free port and
/// releasing it doesn't work: Windows readily hands the same port out again.)
/// Starts below the usual ephemeral ranges, offset per process.
fn next_port() -> u16 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let base = 20_000 + (std::process::id() % 200) * 50;
    loop {
        let port = (base + NEXT.fetch_add(1, Ordering::SeqCst) % 10_000) as u16;
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

/// Minimal HTTP/1.1 exchange on a fresh connection. Returns (status, body).
pub fn http(port: u16, method: &str, path: &str, body: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;

    // Read headers.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof in headers"));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("bad status line: {head:?}")))?;
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);

    // Read body.
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Port of the riffdb.js client. `sql` and `exec` both POST to /query,
/// exactly like connection.ts; `exec` ignores the result.
#[derive(Clone, Copy)]
pub struct Db {
    port: u16,
}

impl Db {
    /// `` sql`...${a}...${b}` `` → `db.sql("...?...?", json!([a, b]))`
    pub fn sql(&self, q: &str, args: Value) -> Result<Vec<Value>, String> {
        let body = json!({ "q": q, "args": args }).to_string();
        let (status, text) = http(self.port, "POST", "/query", &body).map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            if text.is_empty() {
                return Err(format!("HTTP Error: {status}"));
            }
            return Err(text);
        }
        match serde_json::from_str::<Value>(&text).map_err(|e| format!("bad json {text:?}: {e}"))? {
            Value::Array(rows) => Ok(rows),
            other => Err(format!("expected array, got {other}")),
        }
    }

    pub fn exec(&self, q: &str, args: Value) -> Result<(), String> {
        self.sql(q, args).map(|_| ())
    }

    /// Unwrapping shorthands for the common case.
    pub fn q(&self, q: &str) -> Vec<Value> {
        self.sql(q, json!([])).unwrap_or_else(|e| panic!("query failed: {e}\n  sql: {q}"))
    }

    pub fn x(&self, q: &str) {
        self.exec(q, json!([])).unwrap_or_else(|e| panic!("exec failed: {e}\n  sql: {q}"))
    }
}

/// Numeric field as f64 (riffdb returns REAL columns as e.g. `200.0`).
pub fn num(row: &Value, key: &str) -> f64 {
    row[key].as_f64().unwrap_or_else(|| panic!("{key} is not a number in {row}"))
}

pub fn int(row: &Value, key: &str) -> i64 {
    row[key].as_i64().unwrap_or_else(|| panic!("{key} is not an integer in {row}"))
}

pub fn str_<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or_else(|| panic!("{key} is not a string in {row}"))
}
