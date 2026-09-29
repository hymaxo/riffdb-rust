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

const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_IDLE_RESPONSE_CAPACITY: usize = 1 << 20;

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
            // socket is nonblocking, keep pushing
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

pub fn run(worker: ThreadPoolWorker<Request>) {
    let Some(db) = database::open() else {
        worker.stop_pool();
        return;
    };

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

        if response.bytes().is_empty() {
            continue;
        }

        if let Err(e) = send_all(&req.conn.client, response.bytes(), &req.conn.cancel) {
            log_warn!("Cant send data to client: {}", e);
        }

        // don't keep 1mb+ buffers forever
        if response.buf.capacity() > MAX_IDLE_RESPONSE_CAPACITY {
            response.buf = Vec::with_capacity(4096);
        }
    }
}
