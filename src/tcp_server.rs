use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use socket2::{Domain, Socket, Type};
use std::io;
use std::net::SocketAddr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadStatus {
    More,
    Short,
    WouldBlock,
    Closed,
}

const READ_BUFFER_SIZE: usize = 8192;

pub trait TcpServerCallbacks {
    type ClientData;

    fn on_connect(&mut self, client: &std::net::TcpStream) -> Option<Self::ClientData>;
    fn on_readable(&mut self, client: &mut TcpStream, data: &mut Self::ClientData, buf: &mut [u8]) -> ReadStatus;
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
    generations: Vec<u16>,
    free: Vec<usize>,
    max_clients: u16,
    client_count: u16,
    read_buf: Box<[u8]>,
}

fn token(slot: usize, generation: u16) -> Token {
    Token(slot | (generation as usize) << 16)
}

impl<D> TcpServer<D> {
    pub fn create(port: u16, max_clients: u16) -> io::Result<TcpServer<D>> {
        if max_clients == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }

        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;

        // not on windows, there it lets two servers share a port
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
            read_buf: vec![0u8; READ_BUFFER_SIZE].into_boxed_slice(),
        })
    }

    fn remove_client<C: TcpServerCallbacks<ClientData = D>>(&mut self, slot: usize, callbacks: &mut C) {
        let Some(Client { mut stream, data }) = self.clients[slot].take() else {
            return;
        };

        callbacks.on_disconnect(data);

        let _ = self.poll.registry().deregister(&mut stream);
        drop(stream);

        self.generations[slot] = self.generations[slot].wrapping_add(1);
        self.free.push(slot);
        self.client_count -= 1;
    }

    fn accept_clients<C: TcpServerCallbacks<ClientData = D>>(&mut self, callbacks: &mut C) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(_) => break,
            };

            if self.client_count >= self.max_clients {
                drop(stream);
                continue;
            }

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

    pub fn run<C: TcpServerCallbacks<ClientData = D>>(&mut self, callbacks: &mut C) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);

        loop {
            if let Err(e) = self.poll.poll(&mut events, None) {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
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
                    let status = callbacks.on_readable(&mut client.stream, &mut client.data, &mut self.read_buf);
                    if status == ReadStatus::WouldBlock {
                        break;
                    }
                    // windows: reregister instead of one more recv
                    #[cfg(windows)]
                    if status == ReadStatus::Short {
                        let tok = token(slot, self.generations[slot]);
                        if self.poll.registry().reregister(&mut client.stream, tok, Interest::READABLE).is_err() {
                            self.remove_client(slot, callbacks);
                        }
                        break;
                    }
                    if status == ReadStatus::Closed {
                        self.remove_client(slot, callbacks);
                        break;
                    }
                }
            }
        }
    }
}
