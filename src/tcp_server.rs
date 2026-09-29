// Port of TcpServer.h / TcpServer.c
//
// Same design: one thread polls the listening socket and every client,
// accepts new clients up to MaxClients, and calls OnReadable in a loop until
// the socket would block. poll() + pollfd arrays become mio (epoll / kqueue /
// IOCP), the function-pointer callbacks + `void* ClientData` become a trait
// with an associated per-client type.

use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use socket2::{Domain, Socket, Type};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

pub type TcpServerError = i16;
pub const TCP_SERVER_ERROR_EMPTY_READ: TcpServerError = -1;
pub const TCP_SERVER_ERROR_READ: TcpServerError = -2;
/// C: TcpServerErrorRead with errno == EAGAIN/EWOULDBLOCK.
pub const TCP_SERVER_ERROR_WOULD_BLOCK: TcpServerError = -3;
/// Not in C: the read returned less than the buffer size, so the socket is
/// (very likely) empty for now. See the read loop in `run`.
pub const TCP_SERVER_READ_SHORT: TcpServerError = 1;

/// Size of the buffer OnReadable reads into (C: `char Buffer[8192]` on the
/// stack). Allocated once and reused, so it isn't zeroed per read.
pub const TCP_SERVER_READ_BUFFER_SIZE: usize = 8192;

pub trait TcpServerCallbacks {
    type ClientData;

    /// None rejects the client (the socket is closed).
    fn on_connect(&mut self, client: &std::net::TcpStream) -> Option<Self::ClientData>;
    fn on_readable(&mut self, client: &mut TcpStream, data: &mut Self::ClientData, buf: &mut [u8]) -> i16;
    fn on_disconnect(&mut self, data: Self::ClientData);
}

const LISTENER: Token = Token(usize::MAX);

struct Client<D> {
    stream: TcpStream,
    data: D,
}

pub struct TcpServer<D> {
    poll: Poll,
    listener: TcpListener,
    clients: Vec<Option<Client<D>>>,
    /// Bumped whenever a slot is freed, and part of the token, so a stale
    /// event can't reach the next client in the same slot.
    generations: Vec<u16>,
    free: Vec<usize>,
    max_clients: u16,
    client_count: u16,
    running: AtomicBool,
    read_buf: Box<[u8]>,
}

fn token(slot: usize, generation: u16) -> Token {
    Token(slot | (generation as usize) << 16)
}

impl<D> TcpServer<D> {
    /// TcpServerCreate
    pub fn create(port: u16, max_clients: u16) -> io::Result<TcpServer<D>> {
        if max_clients == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }

        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;

        // C: setsockopt(SO_REUSEADDR) and setsockopt(SOL_SOCKET, 15), both
        // unchecked. 15 is SO_REUSEPORT on Linux. On Windows SO_REUSEADDR
        // means something else (a second server may bind the same port), and
        // the Unix meaning is Windows' default, so it isn't set there.
        #[cfg(unix)]
        let _ = socket.set_reuse_address(true);
        #[cfg(target_os = "linux")]
        let _ = socket.set_reuse_port(true);

        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddr::from(([0, 0, 0, 0], port)).into())?;
        socket.listen(128)?;

        let mut listener = TcpListener::from_std(socket.into());
        let poll = Poll::new()?;
        poll.registry().register(&mut listener, LISTENER, Interest::READABLE)?;

        Ok(TcpServer {
            poll,
            listener,
            clients: (0..max_clients).map(|_| None).collect(),
            generations: vec![0; max_clients as usize],
            free: (0..max_clients as usize).rev().collect(),
            max_clients,
            client_count: 0,
            running: AtomicBool::new(false),
            read_buf: vec![0u8; TCP_SERVER_READ_BUFFER_SIZE].into_boxed_slice(),
        })
    }

    /// TcpServerStop
    #[allow(dead_code)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn remove_client<C: TcpServerCallbacks<ClientData = D>>(&mut self, slot: usize, callbacks: &mut C) {
        let Some(Client { mut stream, data }) = self.clients[slot].take() else {
            return;
        };

        callbacks.on_disconnect(data);

        let _ = self.poll.registry().deregister(&mut stream);
        drop(stream); // close(Fd)

        self.generations[slot] = self.generations[slot].wrapping_add(1);
        self.free.push(slot);
        self.client_count -= 1;
    }

    fn accept_clients<C: TcpServerCallbacks<ClientData = D>>(&mut self, callbacks: &mut C) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                // EAGAIN / EWOULDBLOCK or a real error: both break.
                Err(_) => break,
            };

            if self.client_count >= self.max_clients {
                drop(stream); // close(ClientFd)
                continue;
            }

            // mio already made it non-blocking (C: SetNonBlocking).
            let std_stream = std::net::TcpStream::from(stream);
            let Some(data) = callbacks.on_connect(&std_stream) else {
                continue;
            };
            let mut stream = TcpStream::from_std(std_stream);

            let slot = self.free.pop().expect("client_count < max_clients");
            let tok = token(slot, self.generations[slot]);
            if self.poll.registry().register(&mut stream, tok, Interest::READABLE).is_err() {
                self.free.push(slot);
                callbacks.on_disconnect(data);
                continue;
            }

            self.clients[slot] = Some(Client { stream, data });
            self.client_count += 1;
        }
    }

    /// TcpServerRun
    pub fn run<C: TcpServerCallbacks<ClientData = D>>(&mut self, callbacks: &mut C) -> i32 {
        self.running.store(true, Ordering::SeqCst);

        let mut events = Events::with_capacity(1024);

        while self.running.load(Ordering::SeqCst) {
            if let Err(e) = self.poll.poll(&mut events, None) {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return -1;
            }

            for event in events.iter() {
                if event.token() == LISTENER {
                    self.accept_clients(callbacks);
                    continue;
                }

                let slot = event.token().0 & 0xffff;
                let generation = (event.token().0 >> 16) as u16;
                if slot >= self.clients.len() || self.generations[slot] != generation || self.clients[slot].is_none() {
                    continue;
                }

                // C: POLLERR | POLLHUP | POLLNVAL
                if event.is_error() {
                    self.remove_client(slot, callbacks);
                    continue;
                }

                if !event.is_readable() && !event.is_read_closed() {
                    continue;
                }

                loop {
                    let Some(client) = self.clients[slot].as_mut() else {
                        break;
                    };
                    let rc = callbacks.on_readable(&mut client.stream, &mut client.data, &mut self.read_buf);
                    if rc == TCP_SERVER_ERROR_WOULD_BLOCK {
                        break;
                    }
                    // mio is edge-triggered: it only reports the socket again
                    // after a read hits WouldBlock, which is what the C loop
                    // does anyway (read until EAGAIN). On Windows that last
                    // recv() is an extra syscall per request, plus mio's
                    // re-arm; reregister() re-arms without it (the AFD poll
                    // is resubmitted inside the next poll()). On epoll,
                    // reregister is itself a syscall, so keep C's loop there.
                    #[cfg(windows)]
                    if rc == TCP_SERVER_READ_SHORT {
                        let tok = token(slot, self.generations[slot]);
                        if self.poll.registry().reregister(&mut client.stream, tok, Interest::READABLE).is_err() {
                            self.remove_client(slot, callbacks);
                        }
                        break;
                    }
                    if rc == TCP_SERVER_ERROR_EMPTY_READ || rc == TCP_SERVER_ERROR_READ {
                        self.remove_client(slot, callbacks);
                        break;
                    }
                }
            }
        }

        0
    }
}
