use std::io::{ErrorKind, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::database;
use crate::http_response::HttpResponse;
use crate::log::{self, LogVerbosity};
use crate::request::Request;
use crate::router;
use crate::thread_pool::ThreadPoolWorker;
use crate::{log_trace, log_warn};

/// How long a send may make no progress before the response is abandoned.
const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Shrink the response buffer back after a response larger than this, so
/// one huge result doesn't pin memory in an idle worker.
const MAX_IDLE_RESPONSE_CAPACITY: usize = 1 << 20;

/// Writes all of `buf`. The client socket is non-blocking (the network
/// thread polls it), so a full send buffer means backing off and retrying
/// until the client disconnects or stops reading for too long.
fn send_all(client: &TcpStream, mut buf: &[u8], cancel: &AtomicBool) -> std::io::Result<()> {
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
                if cancel.load(Ordering::SeqCst) {
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

/// A worker thread's main loop: take requests from the mailbox, run them,
/// send the responses.
pub fn run(worker: ThreadPoolWorker<Request>) {
    let Some(db) = database::open() else {
        worker.stop_pool();
        return;
    };

    // One buffer per worker, reused for every request.
    let mut response = HttpResponse::new();

    while worker.working() {
        let Some(req) = worker.mailbox().recv() else {
            break;
        };
        if req.conn.cancel.load(Ordering::SeqCst) {
            log_warn!("Request Canceled");
            continue;
        }

        response.zero();
        router::route(&req, &db, &mut response);

        if log::enabled(LogVerbosity::Trace) {
            log_trace!("=== HttpResponse dump ===");
            log_trace!("\n{}", String::from_utf8_lossy(response.bytes()));
            log_trace!("=== end HttpResponse dump ===");
        }

        // Empty when the client disconnected mid-request.
        if response.bytes().is_empty() {
            continue;
        }

        if let Err(e) = send_all(&req.conn.client, response.bytes(), &req.conn.cancel) {
            log_warn!("Cant send data to client: {}", e);
        }

        if response.buf.capacity() > MAX_IDLE_RESPONSE_CAPACITY {
            response.buf = Vec::with_capacity(4096);
        }
    }
}
