# Web port milestone 6: audio and networking in the tab

Execution plan for milestone 6 of `docs/WEB_PORT.md`. Same rules as
`docs/WEB_PORT_M5A_PLAN.md` section 0 (AGENTS.md binding; line endings; one
phase = one commit; gates are the AGENTS.md checkpoint — `cargo test
--workspace`, `tools/clippy_ratchet.py`, and clippy/fmt clean on every file a
phase touches; never edit recorded expected values; stop, report, do not
improvise). Facts below were verified against `e366441`, including a
delegated adversarial review whose measured findings (non-determinism, wasm
time traps, the synchronous music stream) reshaped this plan; its recorded
inputs are cited where they constrain a phase.

## 0. Design

1. **Audio is pure but not yet deterministic or wasm-callable.**
   `skate2-audio` + `skate2-xma` compile for `wasm32-unknown-unknown`
   (verified), but `AudioSystem::new` seeds three generators from the clock
   (`RwRandom(clock_seed())`, `speech::time64_seed()` at system.rs:574-575,
   context `clock_seed()` at :722), and `SystemTime::now`/`Instant::now`
   trap on wasm32 (verified on the pinned 1.99.0). Measured: three native
   runs of the pause-menu corpus test produced three different renders —
   the clock seeds, plus a race with the `rwac stream pth0` music reader
   thread when rendering outruns real time (with the clock pinned by
   interposer: 24/25 idle runs identical, 6/12 under load). Phase 1a makes
   the seeds explicit inputs (native keeps today's clock defaults); the
   oracle pins them.
2. **The oracle is a stream-free render.** The existing test's render reads
   the stream pool — music (`data/audio/music/Ipod_Stream.mus`, hundreds of
   reads per run) — and pool streams cannot run on wasm (no reader threads),
   so any oracle including streams can never match tab to native. Measured
   during phase 1c: `music_mode: OFF` does **not** stop the music stream (the
   reads continue and the hash is unchanged), so the oracle removes the
   streams at the source instead: `skate2_audio::oracle` pins the seeds
   (`ORACLE_SEEDS`) and supplies `AbsentStreams`, a `StreamFiles` whose every
   file is missing — each stream consumer (speech projects, resident samples,
   the pool's chunk reads) answers a missing file with an early return, and
   the pool's reader thread drops the failed request without retrying, so no
   bytes are read and nothing races, on any host, threaded or not. The
   pause-menu **press sound is a splicer bank sound** and renders
   identically with the streams gone. The dump path
   (`SKATE2_AUDIO_DUMP`) proves the render's frame content is
   time-independent once seeds and streams are pinned; the frame hash (over
   `to_bits`) is the oracle, verified bit-stable across fresh processes.
   Cross-target floats: every runtime libm call in the path casts to f32,
   and f32 results matched across macOS and wasm libms in a 2M-input probe —
   f64 `sin`/`powf` etc. differ bit-for-bit but are not observable here. The
   native hash records the platform it was taken on.
3. **The system loader moves into `skate2-audio` as bytes-in.** Today
   `load_system` (game_audio.rs:306) mounts four archives and opens ~25
   audio files by path; every input is bytes except two hidden kinds: the
   loose `.snr`/`.sek` grain files found by `read_dir` (:153-160 — the
   loader gains a recorded const name list), and the clock seeds (§0.1).
   The one lazy reader is the `StreamFiles` trait (rwac/stream_pool.rs:24;
   setter system.rs:663), which is also the synchronous loader for speech
   projects, contexts, posts and `.mpf` (~1.7 MiB per `enter_world` —
   none of it touched once music is off and speech is not played). The
   constructor takes every input by value plus the caller's
   `Arc<dyn StreamFiles>`; `fe_sounds` moves with it over the
   `AttribSource` trait, but `VaultSource` stays app-side (skate2-audio
   does not depend on skate2-core). Native behavior is unchanged.
4. **Audio bootstrap weight, measured: ≈66.4 MiB read** by today's loader
   (69.6 MB synchronous + 90 KB streamed; the synchronous figure includes
   ~11 MiB of AEMS interfaces/banks, 3.5 MiB grains, 1.6 MiB vault). The
   music-off tab render needs only the loader inputs plus a prefetched
   `db.big` span mount for `skater.vlt` (a miss there is a hard error);
   `AttribDb::load` has no clock/thread/fs use and should serve from spans,
   but it has never run in the tab — phase 2 proves it. Narrowing the
   66.4 MiB to a needed-set is the audio-manifest work of a later
   milestone.
5. **Net protocol cores are pure but time-typed.** `ClientCore`
   (client.rs:55) and `ServerCore` (struct server.rs:291) speak
   `receive(bytes, now)` / `update(now)` / `take_outgoing()`; the datagrams
   are opaque sealed bytes and `SocketAddr` is a connection key (its IP
   also keys `MAX_CONNECTIONS_PER_IP = 8`, server.rs:56). The crate
   compiles inside the web app's graph, but `now` is `std::time::Instant`,
   which traps on wasm — phase 5a swaps the crate to `web_time::Instant`
   (the std type on native, unchanged). `tests.rs`'s `Net` harness already
   pins whole sessions in-process with virtual clocks and fixed seeds.
6. **The bridge carries datagrams, not protocol — and is not an open
   relay.** The native server keeps its UDP socket; a std-threaded
   WebSocket bridge (tungstenite, pinned) gives each WebSocket connection
   its own UDP socket (so the server sees distinct addresses) and forwards
   sealed datagrams verbatim. The bridge's UDP target is its own
   configuration, never page-chosen. An https page needs `wss` (TLS in
   front of the bridge), and at most 8 bridged clients share the bridge's
   IP slot — both recorded as deployment notes, not code.
7. **Two server-side facts gate the tab session.** A dev `skate2-server`
   installs a random build secret, and the tab (no build script) presents
   `Build::DEVELOPMENT`, refused with `refuse::BUILD` (server.rs:387) —
   phase 5 settles identity by giving the verification server run an
   explicit build id matched to the tab's, without weakening any
   production check. And frames are acked only for members with a recipe
   (server.rs:1162), with a 2 s inactivity kick (server.rs:1259) — tighter
   than background-tab timers — so the tab client drives `update` on
   `onmessage` as well as its timer, keeps `Busy`/frames flowing, and uses
   the tests' synthetic `frame(tick)` recipe source (honest: not gameplay;
   gameplay frames are milestone 7's).

The proof: in the tab, the pause-menu press sound renders the native frame
hash byte-for-byte with zero stream reads, and is audible through Web
Audio; and a browser client joins a live native server through the bridge —
hello, lobby join, first synthetic frames acked — with the ordered event
sequence a native bridged run records identically (RTTs, addresses and
counts excluded).

## 1. Phases

### Phase 1a — audio: explicit seeds

Files: `crates/skate2-audio/src/system.rs`.

- `AudioSystem::new` and `enter_world` gain explicit seed parameters
  (rwac, speech, contexts); today's clock-derived values become the native
  default so the pc app's behavior is unchanged. No `cfg`.
- Test: two systems built with identical seeds and the no-streams state of
  §0.2 render bit-identical frames across repeated constructions.
- Gate: `cargo test -p skate2-audio`; ratchet; scoped clippy/fmt.

Commit: `Web port step six (a): the audio system takes its seeds explicitly`.

### Phase 1b — audio: the bytes-in loader

Files: `crates/skate2-audio/src/load.rs` (new), its input tables moved from
`apps/skate2-pc/src/game_audio.rs` (splicer/AEMS/HOM/vehicle bank lists,
`EMITTER_DATA_FILES`, the loose `.snr`/`.sek` name list recorded once from
this dump), `fe_sounds` over `AttribSource`; `game_audio.rs` keeps
`VaultSource` and disk reading, delegating assembly.

- The constructor takes every bank/map/grain/AEMS/HOM input by value plus
  the caller's `Arc<dyn StreamFiles>`; nothing reads a path or a clock.
- Gate: pc app corpus tests unchanged in meaning (`--ignored`, dump);
  `cargo test -p skate2-audio -p skate2-pc`; ratchet; scoped clippy/fmt.

Commit: `Web port step six (b): the audio system loads from bytes`.

### Phase 1c — audio: the native oracle

Files: `crates/skate2-audio/src/oracle.rs` (new: `ORACLE_SEEDS`,
`AbsentStreams`, `frame_hash`, the 180-tick script), `apps/skate2-pc/src/
game_audio.rs` (an `oracle_load_system` over `AbsentStreams` and the pinned
test).

- The pause-menu test: pinned seeds, `AbsentStreams`, 180 ticks at 60 Hz,
  and the deterministic frame hash (wrapping FNV over `to_bits`), asserted
  stable across two system builds in one run. Record the hash and the
  platform once; the recorded constant asserts it.
- Gate: the corpus test, run three times, identical hashes; ratchet; scoped
  clippy/fmt.

Commit: `Web port step six (c): the pause-menu render oracle, pinned`.

### Phase 2a — audio: one input assembly for every host

Amended before phase 2 (evidence: 1b moved the *constructor* but left the
archive-level assembly — which archives, which entries, `grains.big` and
`emitters.big` read whole — in `game_audio.rs`, and `VaultSource` app-side;
the tab would have had to copy both, i.e. fork the loader).

Files: `crates/skate2-audio/src/load.rs` (`AudioFiles`: mount an archive /
read a loose file; `read_inputs`, the pc assembly moved unchanged;
`ARCHIVES`, `loose_files`, `wanted_entries` — the entries `read_inputs`
reads, for a ranged host's prefetch), `crates/skate2-core` (`AttribSource`
for `AttribDb` behind an optional `audio` feature — the one vault adapter),
`apps/skate2-pc/src/game_audio.rs` (a disk `AudioFiles`; `VaultSource`
gone), `tools/clippy_ratchet.py`.

- Proof of no native change: the oracle hash is unchanged.
- The ratchet's 1c metric (`generated − duplicates`) is order-dependent:
  the new core→audio edge moved ~2k warnings between `skate2-audio`'s lib
  and test targets on an unchanged tree. It now counts distinct JSON
  diagnostics keyed by crate, primary span, code and message — a set,
  verified identical across forced recompiles on both trees.

Commit: `Web port step six (d0): one audio input assembly for every host`.

### Phase 2 — audio in the tab, headless

Files: `apps/skate2-web/Cargo.toml` (skate2-audio/xma dependencies,
skate2-core's `audio` feature), `src/audio_probe.rs` (new), `src/lib.rs`,
`index.html`; `skate2_audio::oracle` takes the recorded hash so both hosts
assert one constant.

- Open the tables of `load::ARCHIVES` and `db.big` over ranges; make
  exactly `wanted_entries` (and `skater.vlt` plus the `.bin`s it names)
  resident as stored spans with **no miss handler** — a miss is a hard
  error; read the loose files whole. `read_inputs` over span-mounted
  archives, `AttribDb::load` over the span-mounted `db.big`.
- Build through the same constructor with `oracle::AbsentStreams` (call
  counted), play the press key, render the same 180 ticks, assert the
  recorded hash (absent streams by construction: no bytes on either host;
  on wasm the pool's reader-thread spawn fails and the stream closes).
- The host-neutral half (prefetch plan, span mounts, build, render) is
  also run natively by an ignored corpus test over file-range reads: the
  tab's exact configuration minus the fetch transport.
- Panel line `audio probe: N frames, hash matches native`; `?audio=1`
  runs it; real-time CDP driving is the verification route.
- Gate: `cargo test -p skate2-web`; wasm clippy; headless browser run.

Commit: `Web port step six (d): the pause-menu sound renders the native hash in the tab`.

### Phase 3 — audio audible: the Web Audio pull

Files: `apps/skate2-web/src/audio_out.rs` (new; the player),
`src/audio_probe.rs` (split into fetch / build / render so the player
reuses them), `src/lib.rs`, `index.html`, `Cargo.toml` (web-sys
`AudioContext`, `AudioContextState`, `ScriptProcessorNode`,
`AudioProcessingEvent`, `AudioBuffer`, `AudioNode`,
`AudioDestinationNode`, `BaseAudioContext` features);
`crates/skate2-audio/src/output.rs` (new: the pc `Output`'s queue and
linear resampler as `MixResampler`, used by both hosts — amended so the
tab pulls "exactly as `Output::fill` does" without a copy) and
`oracle::pause_menu_press` (the script's press step, which the player
plays).

- A `ScriptProcessorNode` pull on the main thread — the thread the
  single-threaded wasm system lives on; no `SharedArrayBuffer`. The
  callback pulls mix frames exactly as `Output::fill` does (resample only
  if the context rate differs from 48 kHz). A 60 Hz timer drives
  `update(dt)` — a queued sound plays only once updates run. `AudioContext`
  starts suspended: the play trigger's gesture resumes it; headless runs
  verify by the pull counters (blocks pulled, peak, underruns), audibility
  is hand-verified like the picker. The main-thread tradeoff (and the
  AudioWorklet follow-up) is recorded in the code and docs.
- Gate: headless run with counters; ratchet; scoped clippy/fmt.

Commit: `Web port step six (e): the pause-menu sound is audible in the tab`.

### Phase 4 — net: the bridge

Files: `apps/skate2-bridge` (new crate: std threads, one tungstenite
listener thread, one thread + UDP socket per connection; UDP target from
config), `apps/skate2-bridge/tests/` (the parity test, tungstenite
dev-dependency), workspace `Cargo.toml`.

- Each binary WebSocket message is one datagram out, each datagram in one
  binary message; nothing decrypted, reordered or merged.
- The test: two `ClientCore`s with fixed nonces — one over the bridge, one
  direct UDP — against fresh `ServerCore`s with a fixed seed, asserting
  the identical ordered `ClientEvent` sequence with payloads (RTT,
  addresses, counts excluded).
- Gate: `cargo test -p skate2-bridge -p skate2-net`; ratchet; scoped
  clippy/fmt.

Commit: `Web port step six (f): a WebSocket bridge carries the sealed datagrams`.

### Phase 5a — net: the clock seam

Files: `crates/skate2-net/src/*` (`std::time::Instant` → `web_time::Instant`
throughout; `web-time` pinned), `Cargo.toml`.

- On native `web_time::Instant` *is* `std::time::Instant`: zero behavior
  change, proven by the existing `skate2-net` tests. The wasm build stops
  trapping on the first `now`.
- Gate: `cargo test -p skate2-net` (native identical); wasm check in the
  web graph; ratchet; scoped clippy/fmt.

Commit: `Web port step six (g): skate2-net runs on web time`.

### Phase 5b — net in the tab: a live session

Files: `apps/skate2-web/src/online.rs` (new), `src/lib.rs`, `Cargo.toml`
(web-sys `WebSocket`, `BinaryType` features), `index.html`;
`apps/skate2-server` gains a verification-only explicit build id (an env
the milestone's run sets; production defaults untouched).

- The wasm client host: `ClientCore` behind the audio-style mutex; ws
  binary messages feed `receive` and also kick `update(now)`;
  a `setInterval` tick drives `update` + `take_outgoing()`; nonce from
  getrandom; `?server=<bridge url>` and a nickname field; the synthetic
  recipe/frame source of §0.7, kept under the 2 s inactivity window.
- Panel line `online: connected to <bridge> as <nick>, lobby joined,
  frames acked` — the full join sequence, not socket-open.
- Gate: headless run against a live `skate2-server` (explicit build id)
  plus the bridge; the server console corroborates.

Commit: `Web port step six (h): a browser client joins the native server`.

### Phase 6 — docs

`docs/WEB_PORT.md`: rows and status updated; the audio-bootstrap weight and
audio-manifest future work; the ScriptProcessor tradeoff; the seeds and
no-streams oracle recorded as documented web-vs-PC-only deviations; the
bridge deployment notes (`wss`, 8-per-IP); fix the client-transport
TCP→UDP error (:180); milestone 7 next. This plan file marked executed.

Commit: `Web port step six: docs`.

## 2. Stop conditions

- The no-streams render hash differs between two native runs with pinned
  seeds: a real race remains — report both hashes; do not proceed to the
  tab phase on a moving oracle.
- The tab hash differs from the native recording: report with both hashes
  and the recorded native platform; f32 is expected to match (§0.2) — a
  mismatch is a finding, not a tolerance to loosen.
- `skate2_audio::load` cannot express an input without changing native
  behavior: report the input; do not fork the loader.
- A stream byte is read during the oracle: `AbsentStreams` fails every
  `size`, so a delivered byte means the mechanism does not hold — stop and
  report where.
- The bridge must inspect or rewrite datagrams, or take its UDP target
  from the page: stop.
- The build-identity fix weakens a production check: stop and report.
- A native observable changes (pc corpus tests' recorded hashes/counts,
  `skate2-net` test events): stop and report.

## 3. Order and dependencies

```
1a seeds ──► 1b loader ──► 1c oracle ──► 2a assembly ──► 2 tab hash ──► 3 audible ──► 6 docs
4 bridge ──► 5a clock ──► 5b tab session ─────────────────────────────┘
```

Audio and networking are independent; the phases interleave if a session
allows.
