//! Milestone six's networking proof (`docs/WEB_PORT_M6_PLAN.md`, phase 5b):
//! a browser client joins the native server.
//!
//! The tab runs the same `skate2_net::client::ClientCore` the pc game runs,
//! over a WebSocket to `apps/skate2-bridge`, which carries its sealed
//! datagrams to the server's UDP socket verbatim. Binary messages feed
//! `receive` and then pump the core at once, and a 50 ms timer pumps it too:
//! background tabs throttle timers to about a second, so the server's 2 s
//! inactivity kick is met by both. The session is synthetic — a free-skate
//! lobby, a stand-in recipe and frames counting ticks, as the protocol tests
//! use (`skate2-net`'s `tests.rs`); gameplay frames are milestone seven's.
//!
//! The tab presents `Build::DEVELOPMENT` (it has no build script), so the
//! server it joins runs with `SKATE2_SERVER_BUILD=development`, which only a
//! development server build honours.

use skate2_net::{
    client::{ClientCore, ClientEvent},
    crypto::Build,
    frames::{HEADER_WORDS, INTERVAL, NetFrame},
    protocol::{ClientMessage, PlayNow, PlayNowMode, ServerMessage, challenge},
};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, closure::Closure};
use web_time::{Duration, Instant};

/// The free-skate location the session creates its lobby at.
const LOCATION: u64 = 0x55;

/// How the session stands, for the panel.
#[derive(Default)]
struct Progress {
    signed_in: bool,
    /// The joined game and slot.
    joined: Option<(u32, u8)>,
    disconnected: Option<String>,
}

struct Session {
    core: ClientCore,
    socket: web_sys::WebSocket,
    url: String,
    nickname: String,
    progress: Progress,
    next_tick: u32,
    last_frames: Instant,
    last_busy: Instant,
}

impl Session {
    /// Updates the core, sends its datagrams, and reacts to its events.
    fn pump(&mut self) {
        let now = Instant::now();
        if self.progress.signed_in && now - self.last_busy >= Duration::from_millis(250) {
            self.last_busy = now;
            self.core.send(&ClientMessage::Busy);
        }
        // The synthetic replay: a run of `INTERVAL` ticks every 50 ms (60 Hz).
        if self.progress.joined.is_some() && now - self.last_frames >= Duration::from_millis(50) {
            self.last_frames = now;
            let run: Vec<NetFrame> = (self.next_tick..self.next_tick + INTERVAL)
                .map(frame)
                .collect();
            self.next_tick += INTERVAL;
            self.core.send_frames(&run);
        }
        self.core.update(now);
        if self.socket.ready_state() == web_sys::WebSocket::OPEN {
            for datagram in self.core.take_outgoing() {
                let _ = self.socket.send_with_u8_array(&datagram);
            }
        }
        for event in self.core.take_events() {
            match event {
                ClientEvent::SignedIn { .. } => {
                    self.progress.signed_in = true;
                    self.core.send(&ClientMessage::PlayNow(PlayNow {
                        mode: PlayNowMode::Freeskate,
                        ranked: false,
                        challenge_type: challenge::ONLINE_FREE_SKATE,
                        challenge_key: Some(LOCATION),
                        private: false,
                        create: true,
                        fallback_key: LOCATION,
                        game_version: env!("CARGO_PKG_VERSION").into(),
                    }));
                    // Frames are acknowledged only for members with a recipe.
                    self.core.send(&ClientMessage::Recipe(vec![7; 64]));
                }
                ClientEvent::Message(ServerMessage::GameJoined { game, slot, .. }) => {
                    self.progress.joined = Some((game, slot));
                }
                ClientEvent::Disconnected(reason) => {
                    self.progress.disconnected = Some(format!("{reason:?}"));
                }
                _ => {}
            }
        }
    }

    fn line(&self) -> String {
        use std::fmt::Write as _;
        let mut line = format!("online: connected to {} as {}", self.url, self.nickname);
        if let Some(reason) = &self.progress.disconnected {
            let _ = write!(line, ", disconnected: {reason}");
            return line;
        }
        if !self.progress.signed_in {
            line.push_str(", signing in…");
            return line;
        }
        line.push_str(", signed in");
        let Some((game, slot)) = self.progress.joined else {
            line.push_str(", joining a lobby…");
            return line;
        };
        let _ = write!(line, ", lobby joined (game {game}, slot {slot})");
        match self.core.frames_acknowledged() {
            Some(tick) => {
                let _ = write!(
                    line,
                    ", frames acked through tick {tick} of {} sent",
                    self.next_tick
                );
            }
            None => line.push_str(", awaiting frame acks…"),
        }
        if let Some(rtt) = self.core.rtt() {
            let _ = write!(line, ", rtt {} ms", rtt.as_millis());
        }
        line
    }
}

/// A synthetic replay frame: the tick in the header word the protocol tests
/// use, no pose — a stand-in, not gameplay.
fn frame(tick: u32) -> NetFrame {
    let mut header = [0; HEADER_WORDS];
    header[48] = tick;
    NetFrame {
        tick,
        header,
        pose: None,
        twist: [0.0; 4],
        camera: Vec::new(),
        audio_floats: vec![0.0; 64],
        audio_words: vec![0; 16],
    }
}

/// The session's callbacks, kept for the page's lifetime.
struct Keep {
    _session: Rc<RefCell<Session>>,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _open: Closure<dyn FnMut()>,
    _close: Closure<dyn FnMut()>,
    _tick: Closure<dyn FnMut()>,
}

thread_local! {
    static KEEP: RefCell<Option<Keep>> = const { RefCell::new(None) };
}

/// Connects to the bridge at `url` as `nickname` and keeps `line` current.
pub(crate) fn join(url: &str, nickname: &str, line: web_sys::Element) -> Result<(), String> {
    let socket = web_sys::WebSocket::new(url).map_err(|error| format!("{url}: {error:?}"))?;
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
    let nonce = getrandom::u64().map_err(|error| format!("nonce: {error}"))?;
    let now = Instant::now();
    let session = Rc::new(RefCell::new(Session {
        core: ClientCore::new(
            nonce,
            nickname,
            env!("CARGO_PKG_VERSION"),
            Build::DEVELOPMENT,
            now,
        ),
        socket: socket.clone(),
        url: url.to_owned(),
        nickname: nickname.to_owned(),
        progress: Progress::default(),
        next_tick: 0,
        last_frames: now,
        last_busy: now,
    }));
    let message = {
        let session = Rc::clone(&session);
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                return;
            };
            let datagram = js_sys::Uint8Array::new(&buffer).to_vec();
            let mut session = session.borrow_mut();
            session.core.receive(&datagram, Instant::now());
            session.pump();
        })
    };
    let open = {
        let session = Rc::clone(&session);
        Closure::<dyn FnMut()>::new(move || session.borrow_mut().pump())
    };
    let close = {
        let session = Rc::clone(&session);
        let line = line.clone();
        Closure::<dyn FnMut()>::new(move || {
            let mut session = session.borrow_mut();
            if session.progress.disconnected.is_none() {
                session.progress.disconnected = Some("the bridge closed the WebSocket".into());
            }
            line.set_text_content(Some(&session.line()));
        })
    };
    let tick = {
        let session = Rc::clone(&session);
        Closure::<dyn FnMut()>::new(move || {
            let mut session = session.borrow_mut();
            session.pump();
            line.set_text_content(Some(&session.line()));
        })
    };
    socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
    socket.set_onopen(Some(open.as_ref().unchecked_ref()));
    socket.set_onclose(Some(close.as_ref().unchecked_ref()));
    web_sys::window()
        .ok_or("no Window")?
        .set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), 50)
        .map_err(|error| format!("setInterval: {error:?}"))?;
    KEEP.with(|keep| {
        *keep.borrow_mut() = Some(Keep {
            _session: session,
            _message: message,
            _open: open,
            _close: close,
            _tick: tick,
        });
    });
    Ok(())
}
