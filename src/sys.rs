// Thin platform layer (no C counterpart).
//
// The original only builds on POSIX (poll/fcntl/read/close). This module
// exposes the handful of socket calls TcpServer/SocketActions/Worker need,
// backed by libc on Unix and WinSock (WSAPoll) on Windows, so the rest of the
// port can stay a line-by-line translation.

#![allow(dead_code)]

use libc::c_char;
#[cfg(unix)]
use libc::c_void;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::mem::{size_of, zeroed};

    pub type Socket = i32;
    pub type PollFd = libc::pollfd;

    pub const POLLIN: i16 = libc::POLLIN;
    pub const POLLERR: i16 = libc::POLLERR;
    pub const POLLHUP: i16 = libc::POLLHUP;
    pub const POLLNVAL: i16 = libc::POLLNVAL;

    pub unsafe fn init() -> i32 {
        0
    }

    pub fn is_valid(fd: Socket) -> bool {
        fd >= 0
    }

    pub const INVALID_SOCKET: Socket = -1;

    pub unsafe fn socket_tcp() -> Socket {
        libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0)
    }

    pub unsafe fn set_reuse_opts(fd: Socket) {
        let opt: i32 = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &opt as *const i32 as *const c_void,
            size_of::<i32>() as libc::socklen_t,
        );
        // original: setsockopt(ListenFd, SOL_SOCKET, 15, ...) - 15 is
        // SO_REUSEPORT on Linux.
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            15,
            &opt as *const i32 as *const c_void,
            size_of::<i32>() as libc::socklen_t,
        );
    }

    pub unsafe fn set_non_blocking(fd: Socket) -> i32 {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags < 0 {
            return -1;
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return -1;
        }
        0
    }

    pub unsafe fn bind_any(fd: Socket, port: u16) -> i32 {
        let mut addr: libc::sockaddr_in = zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_addr.s_addr = libc::INADDR_ANY.to_be();
        addr.sin_port = port.to_be();
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    }

    pub unsafe fn listen(fd: Socket, backlog: i32) -> i32 {
        libc::listen(fd, backlog)
    }

    pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32 {
        libc::poll(fds, nfds as libc::nfds_t, timeout)
    }

    pub unsafe fn accept(fd: Socket) -> Socket {
        let mut client_addr: libc::sockaddr_in = zeroed();
        let mut addr_len = size_of::<libc::sockaddr_in>() as libc::socklen_t;
        libc::accept(
            fd,
            &mut client_addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
            &mut addr_len,
        )
    }

    pub unsafe fn read(fd: Socket, buf: *mut c_char, len: usize) -> isize {
        libc::read(fd, buf as *mut c_void, len)
    }

    pub unsafe fn send(fd: Socket, buf: *const c_char, len: usize) -> isize {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = 0;
        libc::send(fd, buf as *const c_void, len, flags)
    }

    pub unsafe fn close(fd: Socket) {
        libc::close(fd);
    }

    pub fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    pub fn errno_is_eintr() -> bool {
        errno() == libc::EINTR
    }

    pub fn errno_would_block() -> bool {
        let e = errno();
        e == libc::EAGAIN || e == libc::EWOULDBLOCK
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Networking::WinSock as ws;

    pub type Socket = ws::SOCKET;
    pub type PollFd = ws::WSAPOLLFD;

    pub const POLLIN: i16 = ws::POLLIN as i16;
    pub const POLLERR: i16 = ws::POLLERR as i16;
    pub const POLLHUP: i16 = ws::POLLHUP as i16;
    pub const POLLNVAL: i16 = ws::POLLNVAL as i16;

    pub unsafe fn init() -> i32 {
        let mut data: ws::WSADATA = zeroed();
        ws::WSAStartup(0x0202, &mut data)
    }

    pub fn is_valid(fd: Socket) -> bool {
        fd != ws::INVALID_SOCKET
    }

    pub const INVALID_SOCKET: Socket = ws::INVALID_SOCKET;

    pub unsafe fn socket_tcp() -> Socket {
        ws::socket(ws::AF_INET as i32, ws::SOCK_STREAM, 0)
    }

    pub unsafe fn set_reuse_opts(fd: Socket) {
        // The C code sets SO_REUSEADDR for its Unix meaning: "allow binding
        // while old connections sit in TIME_WAIT". Windows already allows that
        // by default. WinSock's SO_REUSEADDR instead lets a second socket bind
        // a port another socket is *listening* on (two servers silently share
        // one port). SO_EXCLUSIVEADDRUSE makes that second bind fail instead.
        let opt: i32 = 1;
        ws::setsockopt(
            fd,
            ws::SOL_SOCKET,
            ws::SO_EXCLUSIVEADDRUSE,
            &opt as *const i32 as *const u8,
            size_of::<i32>() as i32,
        );
        // SO_REUSEPORT (15 on Linux) has no WinSock equivalent.
    }

    pub unsafe fn set_non_blocking(fd: Socket) -> i32 {
        let mut mode: u32 = 1;
        if ws::ioctlsocket(fd, ws::FIONBIO, &mut mode) != 0 {
            return -1;
        }
        0
    }

    pub unsafe fn bind_any(fd: Socket, port: u16) -> i32 {
        let mut addr: ws::SOCKADDR_IN = zeroed();
        addr.sin_family = ws::AF_INET;
        addr.sin_addr.S_un.S_addr = ws::INADDR_ANY.to_be();
        addr.sin_port = port.to_be();
        ws::bind(
            fd,
            &addr as *const ws::SOCKADDR_IN as *const ws::SOCKADDR,
            size_of::<ws::SOCKADDR_IN>() as i32,
        )
    }

    pub unsafe fn listen(fd: Socket, backlog: i32) -> i32 {
        ws::listen(fd, backlog)
    }

    pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32 {
        ws::WSAPoll(fds, nfds as u32, timeout)
    }

    pub unsafe fn accept(fd: Socket) -> Socket {
        let mut client_addr: ws::SOCKADDR_IN = zeroed();
        let mut addr_len = size_of::<ws::SOCKADDR_IN>() as i32;
        ws::accept(
            fd,
            &mut client_addr as *mut ws::SOCKADDR_IN as *mut ws::SOCKADDR,
            &mut addr_len,
        )
    }

    pub unsafe fn read(fd: Socket, buf: *mut c_char, len: usize) -> isize {
        ws::recv(fd, buf as *mut u8, len as i32, 0) as isize
    }

    pub unsafe fn send(fd: Socket, buf: *const c_char, len: usize) -> isize {
        ws::send(fd, buf as *const u8, len as i32, 0) as isize
    }

    pub unsafe fn close(fd: Socket) {
        ws::closesocket(fd);
    }

    pub fn errno() -> i32 {
        unsafe { ws::WSAGetLastError() }
    }

    pub fn errno_is_eintr() -> bool {
        errno() == ws::WSAEINTR
    }

    pub fn errno_would_block() -> bool {
        errno() == ws::WSAEWOULDBLOCK
    }
}

pub use imp::*;
