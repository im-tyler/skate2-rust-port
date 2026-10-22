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
| Real renderer / session / original assets in the browser | not started — see the blockers below |

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
desktop-only debug binary is not bundled. A release bundle is roughly
0.4 MB of WebAssembly plus 0.1 MB of loader.

The same crate also runs on the desktop as a debug harness for the scene,
where blocking on wgpu's async setup is allowed:

```sh
cargo run -p skate2-web    # from the repository root
```

Validation for the crate: `cargo fmt -p skate2-web -- --check`,
`cargo test -p skate2-web` (12 tests: key ramps, XInput bit mappings,
Gamepad-API-to-XInput conversion, pad platform slots), and
`cargo clippy -p skate2-web --all-targets` on both the host and
wasm32-unknown-unknown targets.

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
2. **Threads** — `std::thread` panics on wasm32-unknown-unknown.
   `skate2-core/src/streaming/resident_collection_load_driver.rs` keeps a
   `std::thread::JoinHandle` for prefetch and
   `owned_resident_collection.rs` spawns workers; on the browser these need a
   cooperative future (`wasm-bindgen-futures`) or a single-threaded load
   path. (True threads via `SharedArrayBuffer` additionally require
   COOP/COEP response headers from the server.)
3. **Networking** — `skate2-net`'s client transport is `std::net` TCP; the
   browser offers WebSockets/WebTransport instead. Randomness is already
   solved (see the getrandom note above). The server stays native.
4. **Audio** — `skate2-audio`/`skate2-xma` are cpal and synchronous decode;
   the browser needs Web Audio with decoded buffers. Pre-converted audio
   packs (option 3 above) would keep XMA decoding offline.
5. **The renderer** — the real render stack lives in `apps/skate2-pc`
   (wgpu, but native windowing and asset plumbing). Moving it to the web app
   is a port of its own; the test scene only validates the substrate.
6. **What already ports cleanly** — `skate2-core` (minus streaming), 
   `skate2-formats` (readers, once fed async bytes) and `skate2-net`'s
   protocol layer compile for wasm32 today; the web crate links core on both
   targets and its clippy/test gates pass.

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
problem.
