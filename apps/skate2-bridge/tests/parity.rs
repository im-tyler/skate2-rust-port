//! The bridge's proof (`docs/WEB_PORT_M6_PLAN.md`, phase 4): one client script
//! run direct over UDP and once through the bridge, each against a fresh
//! `ServerCore` with a fixed seed on a real socket, produces the identical
//! ordered `ClientEvent` sequence — sign-in, the free-skate lobby join, and a
//! run of synthetic frames sent without a disconnect.

use skate2_net::client::{ClientCore, ClientEvent};
use skate2_net::crypto::Build;
use skate2_net::frames::{HEADER_WORDS, INTERVAL, NetFrame};
use skate2_net::protocol::{ClientMessage, PlayNow, PlayNowMode, ServerMessage, challenge};
use skate2_net::server::ServerCore;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// A `ServerCore` on a real UDP socket, as `skate2_net::server::run` drives
/// it, with a fixed seed; stops when dropped.
struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        socket.set_nonblocking(true).expect("nonblocking");
        let addr = socket.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut core = ServerCore::new(7, Build::DEVELOPMENT, Box::new(|_| {}));
            let mut buffer = [0_u8; 2048];
            while !flag.load(Ordering::Relaxed) {
                while let Ok((length, from)) = socket.recv_from(&mut buffer) {
                    core.receive(from, &buffer[..length], Instant::now());
                }
                core.update(Instant::now());
                for (to, datagram) in core.take_outgoing() {
                    let _ = socket.send_to(&datagram, to);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            addr,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where a client's datagrams go.
trait Link {
    fn send(&mut self, datagram: &[u8]);
    fn receive(&mut self) -> Vec<Vec<u8>>;
}

struct Udp(UdpSocket, Vec<u8>);

impl Link for Udp {
    fn send(&mut self, datagram: &[u8]) {
        let _ = self.0.send(datagram);
    }

    fn receive(&mut self) -> Vec<Vec<u8>> {
        let mut datagrams = Vec::new();
        while let Ok(length) = self.0.recv(&mut self.1) {
            datagrams.push(self.1[..length].to_vec());
        }
        datagrams
    }
}

struct Web(tungstenite::WebSocket<TcpStream>);

impl Link for Web {
    fn send(&mut self, datagram: &[u8]) {
        self.0
            .send(tungstenite::Message::Binary(datagram.to_vec().into()))
            .expect("ws send");
    }

    fn receive(&mut self) -> Vec<Vec<u8>> {
        let mut datagrams = Vec::new();
        loop {
            match self.0.read() {
                Ok(tungstenite::Message::Binary(datagram)) => datagrams.push(datagram.to_vec()),
                Ok(other) => panic!("the bridge sent a non-binary message: {other:?}"),
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return datagrams;
                }
                Err(error) => panic!("ws read: {error}"),
            }
        }
    }
}

fn frame(tick: u32) -> NetFrame {
    let mut header = [0; HEADER_WORDS];
    header[48] = tick;
    NetFrame {
        tick,
        header,
        pose: None,
        twist: [0.0; 4],
        camera: Vec::new(),
        audio_floats: vec![0.25; 64],
        audio_words: vec![3; 16],
    }
}

/// The client side of one session: the core, its link, and every event.
struct Session<L> {
    core: ClientCore,
    link: L,
    events: Vec<ClientEvent>,
    last_busy: Instant,
}

impl<L: Link> Session<L> {
    fn new(link: L) -> Self {
        Self {
            core: ClientCore::new(100, "Bridged", "test", Build::DEVELOPMENT, Instant::now()),
            link,
            events: Vec::new(),
            last_busy: Instant::now(),
        }
    }

    /// Runs for `ms`, or until `done` holds over the events so far.
    fn pump_until(&mut self, ms: u64, done: impl Fn(&[ClientEvent]) -> bool) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline && !done(&self.events) {
            let now = Instant::now();
            if now.duration_since(self.last_busy) >= Duration::from_millis(250) {
                self.last_busy = now;
                self.core.send(&ClientMessage::Busy);
            }
            self.core.update(now);
            for datagram in self.core.take_outgoing() {
                self.link.send(&datagram);
            }
            for datagram in self.link.receive() {
                self.core.receive(&datagram, Instant::now());
            }
            self.events.extend(self.core.take_events());
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The script: sign in, create a free-skate lobby, send ten runs of
    /// synthetic frames, disconnect. Returns the ordered events.
    fn run(mut self) -> Vec<String> {
        self.pump_until(5_000, |events| {
            events
                .iter()
                .any(|event| matches!(event, ClientEvent::SignedIn { .. }))
        });
        self.core.send(&ClientMessage::PlayNow(PlayNow {
            mode: PlayNowMode::Freeskate,
            ranked: false,
            challenge_type: challenge::ONLINE_FREE_SKATE,
            challenge_key: Some(0x55),
            private: false,
            create: true,
            fallback_key: 0xabc,
            game_version: "test".into(),
        }));
        self.core.send(&ClientMessage::Recipe(vec![7; 64]));
        self.pump_until(5_000, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    ClientEvent::Message(ServerMessage::GameJoined { .. })
                )
            })
        });
        for run in 0..10 {
            let frames: Vec<NetFrame> = (run * INTERVAL..(run + 1) * INTERVAL).map(frame).collect();
            self.core.send_frames(&frames);
            self.pump_until(45, |_| false);
        }
        self.pump_until(500, |_| false);
        // Acks arrive on the wire's timing, so their tick is not compared,
        // only that the server acknowledged the run on either path.
        assert!(
            self.core.frames_acknowledged().is_some(),
            "no frame acknowledged"
        );
        assert!(
            !self.core.is_closed(),
            "the session closed: {:?}",
            self.events
        );
        // The server draws user ids from the system's random source on
        // purpose (`server.rs`'s `Random`: no player can work out another's
        // id), so the fixed seed does not fix them: the own id reads as a
        // placeholder, which still checks every place it must appear.
        let own = self
            .events
            .iter()
            .find_map(|event| match event {
                ClientEvent::SignedIn { user_id } => Some(user_id.to_string()),
                _ => None,
            })
            .expect("signed in");
        self.events
            .iter()
            .map(|event| format!("{event:?}").replace(&own, "<own user id>"))
            .collect()
    }
}

#[test]
fn a_bridged_client_sees_what_a_direct_client_sees() {
    let direct = {
        let server = Server::start();
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        socket.connect(server.addr).expect("connect");
        socket.set_nonblocking(true).expect("nonblocking");
        Session::new(Udp(socket, vec![0; 65_536])).run()
    };
    let bridged = {
        let server = Server::start();
        let bridge =
            skate2_bridge::Running::start(([127, 0, 0, 1], 0).into(), server.addr).expect("bridge");
        let stream = TcpStream::connect(bridge.addr).expect("tcp");
        let (socket, _) =
            tungstenite::client(format!("ws://{}/", bridge.addr), stream).expect("handshake");
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(1)))
            .expect("timeout");
        Session::new(Web(socket)).run()
    };
    eprintln!("{} events: {direct:#?}", direct.len());
    assert!(direct.iter().any(|event| event.starts_with("SignedIn")));
    assert!(direct.iter().any(|event| event.contains("GameJoined")));
    assert!(!direct.iter().any(|event| event.starts_with("Disconnected")));
    assert_eq!(direct, bridged);
}
