// Port of Worker.h / Worker.c

use libc::c_void;
use libsqlite3_sys::sqlite3;
use std::ptr;
use std::sync::atomic::Ordering;

use crate::database::database_open;
use crate::http_response::http_response_zero;
use crate::request::Request;
use crate::router::router_route;
use crate::sys;
use crate::thread_pool::ThreadPoolWorker;
use crate::xmalloc::xfree;
use crate::http_parser::HttpParser;
use crate::log::{self, LogVerbosity};
use crate::{log_trace, log_warn};

extern "C" {
    // not exposed by libsqlite3-sys' prebuilt bindings
    fn sqlite3_close_v2(db: *mut sqlite3) -> libc::c_int;
}

/// Lossy view of a (ptr, len) C buffer for the trace dump (`%.*s`).
/// Borrows when the bytes are valid UTF-8, so it doesn't allocate.
unsafe fn dump<'a>(p: *const libc::c_char, len: usize) -> std::borrow::Cow<'a, str> {
    if p.is_null() || len == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, len))
}

/// The C worker's "Full parser dump". Kept out of line: it only runs with
/// trace logging on, and it would otherwise bloat the worker loop.
#[cold]
#[inline(never)]
unsafe fn dump_parser(p: *const HttpParser) {
    log_trace!("=== HttpParser dump ===");
    log_trace!("  State          = {}", (*p).state);
    log_trace!("  SawCr          = {}", (*p).saw_cr as i32);
    log_trace!("  SawDoubleDot   = {}", (*p).saw_double_dot as i32);
    log_trace!(
        "  Method         = {} (len={})",
        dump((*p).method.as_ptr(), (*p).method_len as usize),
        (*p).method_len
    );
    log_trace!(
        "  Url            = {} (len={})",
        dump((*p).url.as_ptr(), (*p).url_len as usize),
        (*p).url_len
    );
    log_trace!("  HeadersLen     = {}", (*p).headers_len);
    for i in 0..(*p).headers_len as usize {
        let h = ptr::addr_of!((*p).headers[i]);
        log_trace!(
            "  Header[{}]      = {}: {}",
            i,
            dump((*h).key.as_ptr(), (*h).key_len as usize),
            dump((*h).value, (*h).value_len as usize)
        );
    }
    log_trace!("  BodyStart      = {}", (*p).body_start);
    log_trace!("  BodyCap        = {}", (*p).body_cap);
    log_trace!("  ConsumedBody   = {}", (*p).consumed_body);
    log_trace!("  ContentLength  = {}", (*p).content_length);
    if !(*p).body.is_null() && (*p).content_length > 0 {
        log_trace!("  Body           = {}", dump((*p).body, (*p).content_length as usize));
    } else {
        log_trace!("  Body           = (null or empty)");
    }
    log_trace!("=== end HttpParser dump ===");
}

pub unsafe fn worker_handler(arg: *mut c_void) -> i32 {
    let self_ = arg as *mut ThreadPoolWorker;

    let db = database_open(false);
    if db.is_null() {
        (*(*self_).pool).working.store(false, Ordering::SeqCst);
        return 0;
    }

    while (*(*self_).pool).working.load(Ordering::SeqCst) {
        let req = (*(*self_).mail_box).recv() as *mut Request;
        if (*req).cancel.load(Ordering::SeqCst) {
            log_warn!("Request Canceled");
            continue;
        }

        (*req).worker.db = db;

        // Full parser dump (one level check for the whole block)
        if log::enabled(LogVerbosity::Trace) {
            dump_parser(ptr::addr_of!((*req).state.parser));
        }

        // PORT FIX: C zeroes the response *after* send(). Once send() has
        // handed the bytes to the kernel the client can already issue its
        // next keep-alive request, which round-robin dispatch gives to a
        // *different* worker; that worker appends to this same buffer while
        // this one resets `len` -> corrupted responses with >1 worker.
        // Reset before routing instead; nothing touches it after send().
        http_response_zero(ptr::addr_of_mut!((*req).state.response));

        router_route(req);

        let r = ptr::addr_of_mut!((*req).state.response);

        if log::enabled(LogVerbosity::Trace) {
            log_trace!("=== HttpResponse dump ===");
            log_trace!("\n{}", dump((*r).buf, (*r).len as usize));
            log_trace!("=== end HttpResponse dump ===");
        }

        let rc = sys::send((*req).client_fd, (*r).buf, (*r).len as usize);
        if rc < 0 {
            log_warn!("Cant send data to client: Rc = {}, errno = {}", rc, sys::errno());
        }
    }

    if !db.is_null() {
        sqlite3_close_v2(db);
    }

    xfree(self_ as *mut c_void);
    0
}
