//! The WebSocket-to-UDP bridge (`docs/WEB_PORT_M6_PLAN.md`, phase 4): a browser
//! cannot send UDP, so a tab's client reaches the native server through this.
//!
//! It carries datagrams, not protocol. Each WebSocket connection gets its own
//! UDP socket connected to the configured server — so the server sees one
//! address per browser client, as it would see native clients — and every
//! binary message goes out as one datagram, every datagram comes back as one
//! binary message: nothing decrypted, inspected, reordered or merged. The
//! UDP target is the bridge's own configuration, never chosen by the page,
//! so the bridge is not an open relay.
//!
//! Deployment notes: an https page needs `wss`, so TLS terminates in front of
//! the bridge (a reverse proxy); and every bridged client shares the bridge's
//! IP, so at most `MAX_CONNECTIONS_PER_IP` (8) of them reach one server.

use std::{
    io,
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tungstenite::{Message, WebSocket};

/// How long one WebSocket read waits before the connection's thread checks
/// its UDP socket: the bridge's added latency bound per direction.
const POLL: Duration = Duration::from_millis(2);

/// Larger than any datagram the protocol sends.
const DATAGRAM_BYTES: usize = 65_536;

/// Accepts WebSocket connections on `listener` and bridges each to `server`
/// on its own thread, until `stop` is set (checked between accepts).
///
/// # Errors
/// The listener cannot be polled.
pub fn serve(listener: &TcpListener, server: SocketAddr, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                thread::Builder::new()
                    .name(format!("bridge {peer}"))
                    .spawn(move || {
                        if let Err(error) = bridge(stream, server) {
                            eprintln!("bridge {peer}: {error}");
                        }
                    })?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A bridge serving in the background, for tests and embedding.
pub struct Running {
    /// The WebSocket address it listens on.
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}

impl Running {
    /// Binds `listen` and serves in a background thread.
    ///
    /// # Errors
    /// The address cannot be bound.
    pub fn start(listen: SocketAddr, server: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(listen)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bridge listener".into())
            .spawn(move || serve(&listener, server, &flag))?;
        Ok(Self {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One connection: the WebSocket handshake, then datagrams both ways until
/// either side closes.
fn bridge(stream: TcpStream, server: SocketAddr) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    let mut socket = tungstenite::accept(stream).map_err(io::Error::other)?;
    socket.get_ref().set_read_timeout(Some(POLL))?;
    let local: SocketAddr = if server.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let udp = UdpSocket::bind(local)?;
    udp.connect(server)?;
    udp.set_nonblocking(true)?;
    let mut buffer = vec![0_u8; DATAGRAM_BYTES];
    loop {
        if !web_to_udp(&mut socket, &udp)? {
            return Ok(());
        }
        loop {
            match udp.recv(&mut buffer) {
                Ok(length) => socket
                    .send(Message::Binary(buffer[..length].to_vec().into()))
                    .map_err(io::Error::other)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                // An earlier datagram's ICMP port-unreachable: the server is
                // not up yet or restarted; the client's retries carry on.
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => break,
                Err(error) => return Err(error),
            }
        }
    }
}

/// One WebSocket read (at most [`POLL`]): a binary message becomes one
/// datagram. Returns `false` once the connection is over.
fn web_to_udp(socket: &mut WebSocket<TcpStream>, udp: &UdpSocket) -> io::Result<bool> {
    match socket.read() {
        Ok(Message::Binary(datagram)) => {
            // A failed send is a lost datagram, as on any UDP path.
            let _ = udp.send(&datagram);
            Ok(true)
        }
        // The bridge carries datagrams only.
        Ok(Message::Text(_)) => {
            let _ = socket.close(None);
            Ok(false)
        }
        Ok(Message::Close(_))
        | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
            Ok(false)
        }
        // Pings are answered by tungstenite on the next write or flush.
        Ok(_) => Ok(true),
        Err(tungstenite::Error::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            // Keep pending pongs flowing while idle.
            match socket.flush() {
                Ok(()) => Ok(true),
                Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    Ok(true)
                }
                Err(error) => Err(io::Error::other(error)),
            }
        }
        Err(error) => Err(io::Error::other(error)),
    }
}
