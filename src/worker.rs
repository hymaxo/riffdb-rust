// Port of Worker.h / Worker.c

use std::io::{ErrorKind, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::database::database_open;
use crate::http_response::HttpResponse;
use crate::log::{self, LogVerbosity};
use crate::request::Request;
use crate::router::router_route;
use crate::thread_pool::ThreadPoolWorker;
use crate::{log_trace, log_warn};

/// How long a send may make no progress before the response is abandoned.
const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(10);

/// C calls send() once and ignores short writes, so large responses on a
/// full socket buffer get truncated. The client socket is non-blocking
/// (shared with the poll loop), so on EWOULDBLOCK back off and retry.
fn send_all(client: &TcpStream, mut buf: &[u8], req: &Request) -> std::io::Result<()> {
    let mut stalled_since: Option<Instant> = None;
    let mut spins = 0u32;
    while !buf.is_empty() {
        match (&*client).write(buf) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                stalled_since = None;
                spins = 0;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if req.conn.cancel.load(Ordering::SeqCst) {
                    return Err(ErrorKind::ConnectionAborted.into());
                }
                let since = *stalled_since.get_or_insert_with(Instant::now);
                if since.elapsed() > SEND_STALL_TIMEOUT {
                    return Err(ErrorKind::TimedOut.into());
                }
                spins += 1;
                if spins < 64 {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn worker_handler(self_: ThreadPoolWorker<Request>) -> i32 {
    let Some(db) = database_open(false) else {
        self_.stop_pool();
        return 0;
    };

    // Reused for every request (C: per-connection response buffer inside
    // Request, yyjson buffers per query).
    let mut response = HttpResponse::new();

    while self_.working() {
        let Some(req) = self_.mail_box().recv() else {
            break;
        };
        if req.conn.cancel.load(Ordering::SeqCst) {
            log_warn!("Request Canceled");
            continue;
        }

        // C zeroes the response after send(); see the PORT FIX in the direct
        // port. With a buffer per worker it doesn't matter, but reset first.
        response.zero();

        router_route(&req, &db, &mut response);

        if log::enabled(LogVerbosity::Trace) {
            log_trace!("=== HttpResponse dump ===");
            log_trace!("\n{}", String::from_utf8_lossy(response.bytes()));
            log_trace!("=== end HttpResponse dump ===");
        }

        // Cancelled mid-request: C sends the (empty) response anyway.
        if response.bytes().is_empty() {
            continue;
        }

        if let Err(e) = send_all(&req.conn.client, response.bytes(), &req) {
            log_warn!("Cant send data to client: Rc = -1, errno = {}", e.raw_os_error().unwrap_or(0));
        }

        // Don't let one huge result pin memory in an idle worker.
        if response.buf.capacity() > 1 << 20 {
            response.buf = Vec::with_capacity(4096);
        }
    }

    0
}
