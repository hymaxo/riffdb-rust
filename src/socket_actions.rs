// Port of SocketActions.h / SocketActions.c

use libc::{c_char, c_void};
use std::mem::{size_of, MaybeUninit};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::http_parser::{
    http_parser_free, http_parser_init, http_parser_parse, http_parser_parse_body, HTTP_PARSER_STATE_BODY,
    HTTP_PARSER_STATE_COMPLETE,
};
use crate::http_response::{http_response_free, http_response_init};
use crate::request::Request;
use crate::sys::{self, Socket};
use crate::tcp_server::{TcpServer, TCP_SERVER_ERROR_EMPTY_READ, TCP_SERVER_ERROR_READ, TCP_SERVER_READ_DRAINED};
use crate::thread_pool::ThreadPool;
use crate::xmalloc::{xfree, xmalloc};
use crate::{log_err, log_trace};

pub unsafe fn socket_actions_on_connect(_server: *mut TcpServer, client_fd: Socket, client_data: *mut *mut c_void) {
    log_trace!("Client connected: fd={}", client_fd);

    *client_data = xmalloc(size_of::<Request>());

    let req = *client_data as *mut Request;

    // *Req = (Request){ .ClientFd = ClientFd };
    ptr::write_bytes(req, 0, 1);
    (*req).client_fd = client_fd;
    ptr::write(ptr::addr_of_mut!((*req).cancel), AtomicBool::new(false));

    http_parser_init(ptr::addr_of_mut!((*req).state.parser));
    http_response_init(ptr::addr_of_mut!((*req).state.response));
}

pub unsafe fn socket_actions_on_readable(server: *mut TcpServer, client_fd: Socket, client_data: *mut c_void) -> i16 {
    let req = client_data as *mut Request;

    // Left uninitialised like the C stack buffer: only the first `n` bytes
    // (written by read) are ever looked at. Zeroing it cost a memset of 8 KiB
    // per read.
    let mut buffer = MaybeUninit::<[c_char; 8192]>::uninit();
    let buffer_ptr = buffer.as_mut_ptr() as *mut c_char;
    let n = sys::read(client_fd, buffer_ptr, 8192);

    if n == 0 {
        return TCP_SERVER_ERROR_EMPTY_READ;
    }
    if n < 0 {
        return TCP_SERVER_ERROR_READ;
    }
    // A short read means the socket is drained for now (see TCP_SERVER_READ_DRAINED).
    let ok: i16 = if (n as usize) < 8192 { TCP_SERVER_READ_DRAINED } else { 0 };

    let parser = ptr::addr_of_mut!((*req).state.parser);

    let mut rc = http_parser_parse(parser, n as usize, buffer_ptr);
    if rc < 0 {
        log_err!("body parsing fucked up {}", rc);
    }

    if (*parser).state != HTTP_PARSER_STATE_BODY && (*parser).state != HTTP_PARSER_STATE_COMPLETE {
        return ok;
    }

    rc = http_parser_parse_body(parser, n as usize, buffer_ptr);
    if rc < 0 {
        xfree(req as *mut c_void);
        log_err!("body parsing fucked up {}", rc);
    }

    // PORT FIX: the C version dispatches to a worker even while the body is
    // still incomplete (state == Body), so any body split across multiple
    // read()s gets handled once per chunk with a truncated payload. Only hand
    // the request off once it is complete.
    if (*parser).state != HTTP_PARSER_STATE_COMPLETE {
        return ok;
    }

    (*((*server).user_data as *const ThreadPool)).process(req as *mut c_void);

    ok
}

pub unsafe fn socket_actions_on_disconnect(_server: *mut TcpServer, client_fd: Socket, client_data: *mut c_void) {
    log_trace!("Client disconnected: fd={}", client_fd);

    let req = client_data as *mut Request;
    // NOTE (kept from C): the Request itself is leaked here, and a worker
    // that is still processing it will touch the freed parser/response
    // buffers. To be addressed in the safe rewrite.
    http_parser_free(ptr::addr_of_mut!((*req).state.parser));
    http_response_free(ptr::addr_of_mut!((*req).state.response));

    (*req).cancel.store(true, Ordering::SeqCst);
}
