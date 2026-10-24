# The browser (WebAssembly + WebGPU) port

`apps/skate2-web` is the browser build: a `wasm32-unknown-unknown` target that
compiles `skate2-core` and renders a standalone test scene through wgpu's
WebGPU backend (WebGL2 as the fallback), with the keyboard and the browser's
Gamepad API read through the same `PadPlatform` boundary the native builds
use. It is deliberately **not** the real Skate 2 renderer, session, or asset
pipeline yet; this page records what exists, how to build and run it, and what
blocks a proper port.

## Status

| Goal | State |
| --- | --- |
| WASM + WebGPU build compiling and running | done — verified rendering on the `BrowserWebGPU` backend in Chrome |
| Non-WASM-compatible code gated or replaced | done for the test scene (details below) |
| Build + serve workflow (trunk) | done — `trunk build --release` / `trunk serve` |
| Keyboard mapped to the controller input system | done — `XboxPadSample`s from ramped keys, unit-tested |
| Game bytes in the browser (asset step one) | done — picker and `?http=` sources; all 20 mounted archives found, `globallocators.big` and `db.big` tables parsed in WASM against a real dump |
| Ranged streaming (asset step two) | done — entries read out of archives over HTTP `Range` (or `Blob::slice` for the picker), `RefPack` expanded, hashed sizes validated: `db.big`'s `skater.vlt` (851,632 bytes) read whole through ranged requests |
| Vault attributes (asset step three) | done — the `.vlt`/`.bin` pair parsed in WASM: 675 types, 304 classes, 4,709 definitions, and `physics.default`'s layout read live — gravity `-9.8` (`0xc11ccccd`), the corpus test's pinned word |
| The mount set (asset step four) | done — all twenty archive tables mounted in `RemoteMountSet` (18,274 entries), virtual-path reads routed across archives in mount order: `skater.vlt` via `db.big`, a shader XML via `render.big` |
| Core streaming in the tab (milestone five) | done — the real `SelectedWorldStream` + `SelectedResidentStartup` boot runs single-threaded in the browser over prefetched spans (2.7 MiB, 12 entries) and reports `done — matches native`: 284 high collections, the corpus bounds bits, 21 plans, (21, 80) retained — identical to the native threaded oracle (`docs/WEB_PORT_M5_PLAN.md`) |
| On-demand streaming (milestone 5a) | done — a resident-span miss answers `WouldBlock` instead of failing; the read scheduler retries (`SourceRead::Retry`) and the collection producer reports `Reading` without latching, while the host's miss handler fetches the missed span over HTTP — so a second, unprefetched district collection loads on demand in the tab (1 span, 62 KiB, fetched once; 767 misses retried) with the extended report identical to the native pin (`docs/WEB_PORT_M5A_PLAN.md`) |
| Audio in the tab (milestone six) | done — one bytes-in loader (`skate2_audio::load`) builds the system for the pc app and the tab alike; the pause-menu press sound renders the pinned stream-free oracle `0x76d019a2c241ba4e` (recorded on macOS aarch64, rustc 1.99.0) bit-for-bit in headless Chrome's wasm32, and plays through Web Audio |
| Networking in the tab (milestone six) | done — a browser client joins a live native `skate2-server` through the WebSocket bridge (`apps/skate2-bridge`, event parity with direct UDP): signed in, free-skate lobby joined, synthetic frames acked continuously; `skate2-net` runs on `web_time::Instant` |
| Real renderer / session / original assets in the browser | not started — see the blockers below |

## The port's road

Worked one milestone at a time, each verified against a real dump before it
counts:

| # | Milestone | State |
| --- | --- | --- |
| 1 | Bytes and BIG table parsing in the browser | done |
| 2 | Ranged streaming: entries out of archives, `RefPack`, hashed-size checks | done |
| 3 | Vault attributes: parse the `.vlt` payloads the entries carry | done |
| 4 | The async mount set: all twenty archives mounted in WASM | done |
| 5 | Core streaming in the tab (single-threaded, prefetched) | done |
| 5a | On-demand streaming: a resident-span miss becomes a pending read | done |
| 6 | Audio (Web Audio + XMA) and networking (WebSockets) | done |
| 7 | The real renderer and booting a session in the tab | next |

## Building and running

```sh
cargo install trunk wasm-bindgen-cli --version 0.2.100   # once
cd apps/skate2-web
trunk serve            # dev loop on http://127.0.0.1:8080
trunk build --release  # static bundle into apps/skate2-web/dist
```

Run trunk **from `apps/skate2-web`**: `getrandom 0.3` (pulled in through
`skate2-net`'s cryptography) only builds for wasm32 with the `wasm_js`
feature *and* the `--cfg getrandom_backend="wasm"` flag, which Cargo only
picks up from this directory's `.cargo/config.toml`. The native builds are
untouched by it.

Trunk 0.22 notes (encoded in `Trunk.toml` and `index.html`): `[build] target`
is the HTML entry file, not the Rust triple (wasm32 is implied), and
`index.html` selects the **cdylib** artifact with
`<link data-trunk rel="rust" data-target-name="skate2_web">` so the
desktop-only debug binary is not bundled. A release bundle is about
2.4 MiB of WebAssembly (1.0 MB gzipped) plus 0.1 MB of loader since
milestone six linked the audio and network stacks.

The same crate also runs on the desktop as a debug harness for the scene,
where blocking on wgpu's async setup is allowed:

```sh
cargo run -p skate2-web    # from the repository root
```

Validation for the crate: `cargo fmt -p skate2-web -- --check`,
`cargo test -p skate2-web` (17 tests, among them key ramps, XInput bit
mappings, Gamepad-API-to-XInput conversion, pad platform slots and the boot
manifest parser, plus the ignored audio-probe corpus test that needs
`SKATE2_GAME_ROOT`), and
`cargo clippy -p skate2-web --all-targets` on both the host and
wasm32-unknown-unknown targets.

The milestone-six checks run against a served dump: `?http=<dump base>`
needs a server that answers `Range` requests and sends CORS headers (every
probe reads entries out of archives by range). `?audio=1` (or the "Run
audio probe" button) runs the audio probe; the "Play pause-menu sound"
button is the gesture that resumes the `AudioContext` and plays the press
— the first press fetches and builds the system, later presses play again
— and `?audio=play` presses once on load (a browser's autoplay rule still
wants a gesture unless it is lifted, as in the headless runs). The online
client talks to a native server through the bridge:

```sh
cargo run -p skate2-bridge -- --listen 127.0.0.1:8787 --server <udp addr>
```

and the page is opened with `?server=<bridge url>` (`ws://127.0.0.1:8787`
for the line above, URL-encoded in the query) and an optional `&nick=`, or
the bridge URL and nickname are typed into the fields beside "Join
server". The tab presents the development build, so the server it joins
is a development build started for the verification:

```sh
SKATE2_SERVER_BUILD=development cargo run --release -p skate2-server -- 127.0.0.1:32082
```

A release build of the server ignores the variable; without it a
development server refuses the tab (`refuse::BUILD`).

## What the browser build contains

- `src/lib.rs` — the `#[wasm_bindgen(start)]` entry: finds `#skate2-canvas`,
  reports a plain-language message when the browser has no `navigator.gpu`,
  initialises wgpu asynchronously (there is no blocking pool and `Instant` is
  not implemented on this target — the clock is `Performance`, falling back to
  `Date`), then runs the frame loop. The desktop entry beside it uses
  `pollster`.
- `src/scene.rs` + `src/scene.wgsl` — a plaza (checkerboard, ledge, rail) and
  a demo deck that answers the pad: left stick carves, right stick tilts,
  A hops. Not Skate 2 gameplay or a Skate 2 material; it proves the
  winit 0.29 / wgpu 30 / WebGPU pairing renders from WASM. Device limits are
  `downlevel_defaults()` so the WebGL2 fallback can also serve weak GPUs —
  the seed of a lighter graphics mode.
- `src/input.rs` — the browser pad platform. The keyboard becomes a pad with
  ramped analog channels; the Gamepad API's standard mapping becomes an
  `XboxPadSample` with the integer semantics the console's `XInput` had
  (symmetric ±32767 axes, as the native SDL platform's converted axes also
  use; browser Y-down is negated). Firefox's axes-4/5 triggers are folded in.
  Vibration is tracked for the debug HUD; browser rumble
  (`GamepadHapticActuator`) is future work.
- `src/game_files.rs` — asset step one: the page's byte sources for the
  game's files. A folder picker (`webkitdirectory`) keeps the player's own
  dump in their browser, and `?http=<base>` fetches from a served copy
  (the verification route and the shape of the future streaming option).
  The assets panel checks all twenty mounted archive paths and parses the
  two smallest self-contained tables (`globallocators`, `db` — a `Viv4`
  archive's `.bh` is read when the `.big` is data only) through the real
  `skate2_formats` parser in WASM.
- `src/audio_probe.rs` — milestone six's audio oracle in the tab, split into
  fetch / build / render so the player reuses it (below).
- `src/audio_out.rs` — the Web Audio player: a `ScriptProcessorNode` pull
  over `skate2_audio::output::MixResampler`, the same queue and linear
  resampler the pc's cpal `Output` uses.
- `src/online.rs` — the online client host: `ClientCore` over a
  WebSocket to the bridge, a synthetic free-skate session (see the
  networking section below).

### Keyboard mapping

| Keys | Pad |
| --- | --- |
| W A S D | left stick (steer) |
| Arrow keys | right stick (flick/tilt) |
| Space | A |
| Shift | B |
| Q / E | left / right trigger |
| Z / C | left / right shoulder |
| Enter | Start |
| Tab | Back |
| 1 / 2 | left / right stick press |

The keyboard slot reports the fixed packet 7, as the native demo-sample path
does for a synthetic controller zero.

## The asset loading problem

The native game reads the player's own extracted Skate 2 files: `SKATE2_GAME_ROOT`
(or the `.xex` the player picks at first launch, validated by
`apps/skate2-pc/src/game_root.rs`) names a folder, and the BIG archives,
Vault data and part databases under it are read synchronously with `std::fs`.
A browser page cannot open a local folder that way — there is no synchronous
file system, no arbitrary paths, and the repository rule that original game
assets stay outside the repository applies to a served site just as it does to
a git checkout. The realistic options, in the order they should be tried:

1. **File System Access API** (`showDirectoryPicker`): the player points the
   browser at their extracted dump once; files stream in on demand and the
   handle persists in IndexedDB for later visits. Chrome/Edge only; Firefox
   and Safari would need option 2.
2. **`<input type="file" webkitdirectory>`**: works everywhere, but the whole
   selection is handed over in one shot, so the multi-gigabyte dump would have
   to be copied into memory or Origin Private File System storage before the
   game starts.
3. **Self-hosted converted packs**: the formats crate already parses BIG and
   Vault; an offline step could emit compact web bundles (meshes, textures
   pre-transcoded to GPU-friendly formats) that a page fetches over HTTP.
   This is the streaming-converted-data route of other browser ports. Whoever
   serves the page answers for the assets; the repository ships none.
4. **Clean-room regenerated assets**: furthest out; only what the
   reconstruction can express without original data.

Whatever carries the bytes, the reader side must become `async`: the
formats/streaming code is synchronous today, and on wasm32 `std::fs` calls
compile but always fail at runtime.

## Blockers for a proper port

In rough dependency order, with the code that has to move:

1. **Asset pipeline** (above) — `skate2-formats` readers and
   `skate2-core/src/streaming/` are `std::fs`, synchronous. Everything else on
   this list is secondary until a session can load a world.
2. **Threads** — resolved for the streaming path by milestone five, not
   by porting threads but by removing the need: a host policy
   (`host_thread::disable_worker_threads`) makes the four optional workers
   run inline, the read scheduler gained an inline mode with the same pool,
   admission and FIFO, and the boot's bytes are prefetched to memory
   (`MountedBig::with_storage` over `ResidentSpans`) because core reads
   synchronously everywhere. Since milestone 5a the prefetch is a floor,
   not a ceiling: a read outside the resident spans answers
   `ErrorKind::WouldBlock`, a host miss handler fetches the span on
   demand, and the scheduler/producer retry until it lands — the boot
   manifest is the bootstrap prefetch only, not a closure of what a
   session will read. A native corpus test pair pins threaded and
   threadless boots to identical reports. Remaining `std::thread` users —
   the animation job pool, the AI/scene/living-world loaders,
   `finish_initial_load`'s sleep loop, remote skater, career saves, the RNG
   seed and `diagnostics` clock — are session-boot (milestone seven)
   concerns, off the streaming path.
3. **Networking** — resolved for joining by milestone six. The client
   transport is UDP: sealed datagrams driven through the pure
   `ClientCore` (`receive(bytes, now)` / `update(now)` /
   `take_outgoing()`), and a page cannot open a UDP socket. The server
   stays native; `apps/skate2-bridge` carries the datagrams verbatim over
   WebSockets, and `skate2-net` takes its clock from `web_time::Instant`
   (the std type on native) so the cores no longer trap on wasm32.
   Randomness was already solved (the getrandom note above). What remains
   is gameplay frames over the session — milestone seven.
4. **Audio** — resolved for the press-sound path by milestone six, without
   pre-converted packs: `skate2-audio`/`skate2-xma` run in the tab as-is,
   decoding XMA in WASM, and a Web Audio pull replaces cpal. Streams
   (music, speech) do not run in the tab yet (below), and the bootstrap
   is heavy.
5. **The renderer** — the real render stack lives in `apps/skate2-pc`
   (wgpu, but native windowing and asset plumbing). Moving it to the web app
   is a port of its own; the test scene only validates the substrate.
6. **What already ports cleanly** — `skate2-core` (minus streaming), 
   `skate2-formats` (readers, once fed async bytes), `skate2-audio`,
   `skate2-xma` and `skate2-net`'s protocol layer compile for wasm32 today;
   the web crate links core on both targets and its clippy/test gates pass.

Verification so far: the release bundle loads in Chrome, initialises the
`BrowserWebGPU` adapter, presents the plaza (canvas pixels match the lit
checkerboard — about 70% of the frame is ground, the rest sky and HUD panels),
and the HUD shows the keyboard pad; no console errors. The projection is
right-handed with a 0..1 depth range (`glm::perspective_rh_zo` semantics),
pinned by unit tests; its first draft was left-handed against a right-handed
camera, which clipped every triangle and left only the clear color — a
reminder that "the canvas presents" is not "the scene renders". The `fps`
counter needs a visible, focused tab — under headless virtual time the
rAF-driven winit loop does not tick, which is a testing artifact, not a scene
problem. The assets panel is verified the same way: with `?http=` aimed at a
served extracted dump it reports 20/20 archives and parses
`globallocators.big` (18 entries) and `db.big` (`skater.vlt`/`.bin`,
`ChallengeCore.vlt`/`.bin`) in WASM; the picker needs a human's folder choice
and so is hand-tested. Step two is verified the same way: the ranged read
line shows `skater.vlt`'s 851,632 bytes arriving whole through ranged
requests (a 206 from the range-aware test server; a Range-ignoring server
falls back to slicing the whole response, as the code does explicitly).
Step three is verified against the game's own corpus oracle: the panel
parses the whole Vault (675 types, 304 classes, 4,709 definitions — this
NTSC dump's `skater.vlt` is 851,632 bytes where the corpus researcher's
build was 851,568, a dump difference, but) and reads `physics`'s `default`
collection layout live — gravity `-9.8`, bits `0xc11ccccd`, exactly the
word the corpus test pins. Class names are verified by executable-pinned
hashes (`physics`, `fe`, `characters_marquee`); the `.bin` string table
does not carry them. Step four mounts the same set: twenty tables, 18,274
entries, every read routed by virtual path — `skater.vlt` served from
`db.big`, `data\render\shaders\debug\advertisement.xml` from `render.big`.

Milestone five's boot probe and milestone 5a's on-demand extension are
verified the same way, against a served dump. The probe drives the real
`SelectedWorldStream` + `SelectedResidentStartup` boot over the prefetched
spans; 5a then requests a second district collection the manifest does not
name. Its reads miss, and the miss becomes a pending read: `ResidentSpans`
answers `WouldBlock` with the host's fetch kick called outside the span
lock, the scheduler's `Retry` and the producer's `Reading` hold the line,
and in the tab the fetched span lands and the load completes. The native
pin (both corpus tests, threaded and threadless, over span mounts with a
blocking handler) records the extended report — 284 high collections, the
corpus bounds bits, 21 + 7 plans, (28, 124) retained at the second
completion — and the tab's final panel line matches it: `boot probe:
done — matches native; … on-demand: 1 spans (62 KiB) fetched` (boot 0.2 s,
longest poll 156 ms). The served-dump access log carries the proof that
nothing was prefetched for it: one ranged fetch of `worldbam.big` bytes
479363584-479427583 — an entry no manifest names. One testing note for
future gates: drive the browser check with real time; headless
`--virtual-time-budget` starves the cooperative `setTimeout` loop and
freezes the panel mid-boot.

## Milestone six: audio and networking

Planned and executed per `docs/WEB_PORT_M6_PLAN.md`; the commit bodies
carry the measurements quoted here.

### Audio

The system loader moved into `skate2-audio` as bytes-in: `skate2_audio::load`
holds the bank/interface tables, the recorded loose `.snr`/`.sek` name list
(a byte source cannot enumerate a directory), `fe_sounds` over
`AttribSource`, and `read_inputs` — the archive-level assembly — over an
`AudioFiles` host trait. The pc app supplies a disk `AudioFiles`, the tab
span-mounted archives; `skate2-core` implements `AttribSource` for
`AttribDb` behind an optional `audio` feature, so there is one vault
adapter and no forked loader. Native behavior was proven unchanged at each
step (every input count and byte, all 168 fe leaves, the assembly order,
the oracle hash).

The oracle is the pause-menu press: pinned seeds, absent streams, 180 ticks
at 60 Hz, FNV-1a over the frames' f32 bits. It was recorded as
`0x76d019a2c241ba4e` on macOS aarch64 with rustc 1.99.0 over the NTSC dump,
bit-identical across three fresh processes, and both hosts assert the one
constant (`oracle::PAUSE_MENU_PRESS_HASH`). The tab reproduces it:

```
audio probe: 144128 frames, hash matches native (0x76d019a2c241ba4e);
2081 stream opens answered absent, 0 stream reads, 0 span misses;
fetched 64.6 MiB in 0.8s, built and rendered in 0.2s
```

(headless Chrome 155, `trunk --release`, three fresh loads identical) —
the first cross-host determinism proof of the audio stack: aarch64 native
and wasm32 agree to the bit. The tab makes exactly `load::wanted_entries`
(plus `skater.vlt` and the `.bin`s it names) resident with no miss handler,
so an unplanned read is a hard error; an ignored native corpus test runs
the same planned spans minus the fetch transport, which is what proves the
entry list complete.

Audible: `src/audio_out.rs` pulls 2048-frame blocks through a
`ScriptProcessorNode`, and a 60 Hz `setInterval` drives `update(1/60)` in
whole ticks against the page clock (at most four per firing). Headless
Chrome 155, autoplay gesture lifted: `audio out: Running at 44100 Hz
(2048-frame main-thread pulls); 141 blocks pulled, peak 0.1172, 0 late; 1
presses` (44.1 kHz resampled from the 48 kHz mix; the native 48 kHz render
peaks at 0.1209). Audibility itself is a hand check, like the picker.

**The main-thread tradeoff.** `ScriptProcessorNode` is deprecated and runs
on the main thread — the thread the single-threaded wasm system lives on,
which is why it was chosen: no `SharedArrayBuffer` is needed. A long
main-thread task makes a block late (counted as `late`). The follow-up is
an `AudioWorklet` reading a `SharedArrayBuffer` ring, which needs
cross-origin isolation (COOP/COEP headers on the page) and the audio system
moved to a worker.

**Bootstrap weight.** The tab fetches 64.6 MiB for the oracle — the
loader's whole input set (the native loader reads ≈66.4 MiB), far more than
the press sound uses. Narrowing it to a needed set is the audio-manifest
work of a future milestone.

### Web-vs-PC deviations

Documented exceptions, not parity claims:

| Deviation | PC | Web |
| --- | --- | --- |
| Generator seeds (music mftb, speech `time64`, speech context) | drawn from the clock, as before | explicit inputs; the oracle pins `ORACLE_SEEDS` (`SystemTime`/`Instant` trap on wasm32) |
| Streams (music, speech projects, the pool's chunk reads) | read by the stream pool's reader threads | absent: the oracle runs with `AbsentStreams` on both hosts; on wasm the pool's reader-thread spawn fails and the stream closes, rendering identically to native's dropped requests |

The oracle needs both: the clock seeds made three native renders differ,
and the music reader thread races the render when it outruns real time (a
`music_mode` of OFF does not stop the music stream). Music and speech
cannot run on wasm today — there are no reader threads — so a web session
has none until the stream pool gains a threadless path.

### Networking

`apps/skate2-bridge` (std threads, `tungstenite =0.30.0`, no TLS) gives each
WebSocket connection its own thread and its own UDP socket connected to the
configured server, so the server sees one address per browser client. Each
binary message is one datagram out, each datagram one binary message back —
nothing decrypted, inspected, reordered or merged; text frames close the
connection. Its parity test runs one client script (sign in, create a
free-skate lobby, ten runs of synthetic frames) direct and through the
bridge against fresh `ServerCore`s on real sockets; the ordered events are
identical, 5 runs of 5: `SignedIn`, `PlayNowResult 0`, `GameJoined` (game
1, slot 0, `LobbySkate`, the client itself as player 0), `ChatEnabled`. A
fixed server seed does not fix user ids — the server draws them from the
system's random source on purpose — so the client's own id is compared as a
placeholder.

`skate2-net` takes its `Instant` from `web-time` (`=1.1.0`): the std type on
native (all 72 `skate2-net` tests pass with no expected value edited), a
`performance.now()` clock on wasm32.

The browser client joins the native server through the bridge.
`src/online.rs` holds `ClientCore` in an `Rc<RefCell>` on the main thread.
Binary WebSocket messages feed `receive` and pump the core at once, and a
50 ms timer pumps it too: even throttled to a background tab's ~1 s, the
timer stays inside the server's 2 s inactivity kick, and the message path
keeps the core served regardless. On sign-in the client creates a free-skate
lobby and sends a stand-in recipe (frames are acked only for members with
one); once joined it sends `INTERVAL` synthetic frames every 50 ms, ticks
counted in the header word the protocol tests use — not gameplay.
Identity: the tab has no build script and presents `Build::DEVELOPMENT`,
which a development server's random secret refuses. `SKATE2_SERVER_BUILD=
development` makes a development server take the development build for
the verification; a release build — one carrying its update public key —
reports and ignores it, so no production check is weakened. Headless
Chrome 155, release builds, a live `skate2-server` and the bridge:

```
online: connected to ws://127.0.0.1:8787 as TabSkater, signed in,
lobby joined (game 1, slot 0), frames acked through tick 347 of 351
sent, rtt 9 ms
```

with acks advancing for the whole run, well past the 2 s kick, and the
server console corroborating each step (`signed in as TabSkater`, `game 1
created: type 8 challenge 0000000000000055`, `TabSkater joined game 1 in
slot 0`). The other direction holds too: the same tab against a server
without the variable is refused with `Refused(4)` (`refuse::BUILD`).
`ClientCore::frames_acknowledged` (read-only, no behavior change) makes
the acks observable; the bridge parity test asserts one on both paths.

**Bridge deployment notes.** An https page needs `wss`, so TLS is
terminated in front of the bridge (the bridge has none). Bridged clients
share the bridge's IP and the server admits at most 8 connections per IP
(`MAX_CONNECTIONS_PER_IP`), so at most 8 bridged clients reach one server.
The UDP target is the bridge's `--server` configuration, never chosen by
the page, so the bridge is not an open relay. Each connection polls — a
WebSocket read with a 2 ms timeout, then the UDP socket drained
non-blocking — which is fine for verification and should be revisited for
deployment.

**Remaining wasm hazard.** `skate2_net::server::run` seeds from
`SystemTime::now()`. It is the native server's entry point only; a server
running in a tab would need the same swap the clock got.

### Next: milestone seven

Gameplay frames over the session: the synthetic frame source is milestone
six's stand-in, replaced by real frames from a session booted in the tab
alongside the real renderer.
