// Port of TcpServer.h / TcpServer.c
//
// Single-threaded poll() loop. Slot 0 of `poll_fds` is the listening socket,
// slot i+1 belongs to client i (whose opaque data lives in clients_data[i]).

use libc::c_void;
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sys::{self, PollFd, Socket, POLLERR, POLLHUP, POLLIN, POLLNVAL};
use crate::xmalloc::{xcalloc, xfree};

pub type TcpServerError = i16;
pub const TCP_SERVER_ERROR_EMPTY_READ: TcpServerError = -1;
pub const TCP_SERVER_ERROR_READ: TcpServerError = -2;
/// Not in C: OnReadable read fewer bytes than its buffer holds, so the socket
/// is drained for now. poll() is level-triggered and reports the fd again if
/// more data arrives, so stop reading here instead of spending one more
/// recv() just to get EWOULDBLOCK (saves a syscall per request).
pub const TCP_SERVER_READ_DRAINED: TcpServerError = 1;

pub type TcpServerOnConnect = unsafe fn(server: *mut TcpServer, client_fd: Socket, client_data: *mut *mut c_void);
pub type TcpServerOnReadable = unsafe fn(server: *mut TcpServer, client_fd: Socket, client_data: *mut c_void) -> i16;
pub type TcpServerOnDisconnect = unsafe fn(server: *mut TcpServer, client_fd: Socket, client_data: *mut c_void);

pub struct TcpServer {
    pub listen_fd: Socket,
    pub poll_fds: *mut PollFd,
    pub clients_data: *mut *mut c_void,
    pub max_clients: u16,
    pub client_count: u16,
    pub running: AtomicBool,
    pub on_connect: Option<TcpServerOnConnect>,
    pub on_readable: Option<TcpServerOnReadable>,
    pub on_disconnect: Option<TcpServerOnDisconnect>,
    pub user_data: *mut c_void,
}

impl TcpServer {
    pub const fn zeroed() -> TcpServer {
        TcpServer {
            listen_fd: sys::INVALID_SOCKET,
            poll_fds: ptr::null_mut(),
            clients_data: ptr::null_mut(),
            max_clients: 0,
            client_count: 0,
            running: AtomicBool::new(false),
            on_connect: None,
            on_readable: None,
            on_disconnect: None,
            user_data: ptr::null_mut(),
        }
    }
}

unsafe fn remove_client(server: *mut TcpServer, index: u16) {
    let fd = (*(*server).poll_fds.add(index as usize + 1)).fd;
    let client_data = *(*server).clients_data.add(index as usize);

    ((*server).on_disconnect.unwrap())(server, fd, client_data);

    sys::close(fd);

    let last = (*server).client_count - 1;
    if index != last {
        *(*server).clients_data.add(index as usize) = *(*server).clients_data.add(last as usize);
        *(*server).poll_fds.add(index as usize + 1) = *(*server).poll_fds.add(last as usize + 1);
    }

    (*server).client_count -= 1;
}

pub unsafe fn tcp_server_create(server: *mut TcpServer, port: u16, max_clients: u16) -> i32 {
    if server.is_null() || max_clients == 0 {
        return -1;
    }

    let mut is_error = false;
    let listen_fd: Socket;

    'body: {
        ptr::write(server, TcpServer::zeroed());
        (*server).max_clients = max_clients;
        (*server).listen_fd = sys::INVALID_SOCKET;

        (*server).poll_fds = xcalloc(max_clients as usize + 1, size_of::<PollFd>()) as *mut PollFd;
        (*server).clients_data = xcalloc(max_clients as usize, size_of::<*mut c_void>()) as *mut *mut c_void;

        listen_fd = sys::socket_tcp();
        if !sys::is_valid(listen_fd) {
            is_error = true;
            break 'body;
        }

        sys::set_reuse_opts(listen_fd);

        if sys::set_non_blocking(listen_fd) < 0 {
            is_error = true;
            break 'body;
        }

        if sys::bind_any(listen_fd, port) < 0 {
            is_error = true;
            break 'body;
        }

        if sys::listen(listen_fd, 128) < 0 {
            is_error = true;
            break 'body;
        }
    }
    // error:
    if is_error {
        if sys::is_valid(listen_fd) {
            sys::close(listen_fd);
        }
        xfree((*server).poll_fds as *mut c_void);
        xfree((*server).clients_data as *mut c_void);

        return -1;
    }

    (*server).listen_fd = listen_fd;
    (*(*server).poll_fds).fd = listen_fd;
    (*(*server).poll_fds).events = POLLIN;
    (*server).running.store(false, Ordering::SeqCst);

    0
}

pub unsafe fn tcp_server_destroy(server: *mut TcpServer) {
    if server.is_null() {
        return;
    }

    tcp_server_stop(server);

    for i in 0..(*server).client_count as usize {
        sys::close((*(*server).poll_fds.add(i + 1)).fd);
    }

    if sys::is_valid((*server).listen_fd) {
        sys::close((*server).listen_fd);
        (*server).listen_fd = sys::INVALID_SOCKET;
    }

    xfree((*server).poll_fds as *mut c_void);
    xfree((*server).clients_data as *mut c_void);
    (*server).poll_fds = ptr::null_mut();
    (*server).clients_data = ptr::null_mut();
    (*server).client_count = 0;
}

pub unsafe fn tcp_server_set_callbacks(
    server: *mut TcpServer,
    on_connect: TcpServerOnConnect,
    on_readable: TcpServerOnReadable,
    on_disconnect: TcpServerOnDisconnect,
    user_data: *mut c_void,
) {
    if server.is_null() {
        return;
    }
    (*server).on_connect = Some(on_connect);
    (*server).on_readable = Some(on_readable);
    (*server).on_disconnect = Some(on_disconnect);
    (*server).user_data = user_data;
}

pub unsafe fn tcp_server_stop(server: *mut TcpServer) {
    if !server.is_null() {
        (*server).running.store(false, Ordering::SeqCst);
    }
}

pub unsafe fn tcp_server_run(server: *mut TcpServer) -> i32 {
    if server.is_null() || !sys::is_valid((*server).listen_fd) {
        return -1;
    }

    (*server).running.store(true, Ordering::SeqCst);

    while (*server).running.load(Ordering::SeqCst) {
        let nfds = (*server).client_count as usize + 1;
        let ready = sys::poll((*server).poll_fds, nfds, -1);

        if ready < 0 {
            if sys::errno_is_eintr() {
                continue;
            }
            return -1;
        }

        if ready == 0 {
            continue;
        }

        if (*(*server).poll_fds).revents & (POLLIN | POLLERR | POLLHUP) != 0 {
            loop {
                let client_fd = sys::accept((*server).listen_fd);

                if !sys::is_valid(client_fd) {
                    // EAGAIN / EWOULDBLOCK or a real error: both break.
                    break;
                }

                if (*server).client_count >= (*server).max_clients {
                    sys::close(client_fd);
                    continue;
                }

                if sys::set_non_blocking(client_fd) < 0 {
                    sys::close(client_fd);
                    continue;
                }

                let idx = (*server).client_count as usize;

                *(*server).clients_data.add(idx) = ptr::null_mut();
                let pfd = (*server).poll_fds.add(idx + 1);
                (*pfd).fd = client_fd;
                (*pfd).events = POLLIN;
                (*pfd).revents = 0;

                (*server).client_count += 1;

                ((*server).on_connect.unwrap())(server, client_fd, (*server).clients_data.add(idx));
            }
        }

        let mut i = (*server).client_count as i32 - 1;
        while i >= 0 {
            let iu = i as usize;
            let rev = (*(*server).poll_fds.add(iu + 1)).revents;
            if rev == 0 {
                i -= 1;
                continue;
            }

            let fd = (*(*server).poll_fds.add(iu + 1)).fd;
            let client_data = *(*server).clients_data.add(iu);

            if rev & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                remove_client(server, i as u16);
                i -= 1;
                continue;
            }

            if rev & POLLIN != 0 {
                loop {
                    let rc = ((*server).on_readable.unwrap())(server, fd, client_data);
                    if rc == TCP_SERVER_READ_DRAINED {
                        break;
                    }
                    if rc == TCP_SERVER_ERROR_EMPTY_READ {
                        remove_client(server, i as u16);
                        break;
                    }
                    if rc == TCP_SERVER_ERROR_READ {
                        if sys::errno_would_block() {
                            break;
                        }
                        remove_client(server, i as u16);
                        break;
                    }
                }
            }

            i -= 1;
        }
    }

    0
}
