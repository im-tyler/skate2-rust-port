# Web port milestone 7: the game in the tab, on Web Worker threads

Execution plan for milestone 7 of `docs/WEB_PORT.md`. Same rules as
`docs/WEB_PORT_M5_PLAN.md` section 0 (AGENTS.md binding; mixed line endings
kept; no `cfg(target_arch)` in `skate2-core` or `skate2-formats` — browser
differences are host policy that also runs on native; one phase = one
commit; never edit recorded expected values; game data never committed;
stop, report, do not improvise). Gates per phase: `cargo test --workspace`
(stable 1.99, unchanged), `tools/clippy_ratchet.py`, clippy/fmt clean on
touched hunks, the threaded wasm build from `apps/skate2-web`, and the
corpus tests a phase names.

## 0. The route, and why

A first draft planned a single-threaded tab session proven bit-identical to
native at every step. An adversarial review found its core gate unsound —
core physics calls `sin`/`acos`/`atan2` ~140 times and native macOS uses
Apple's libm while wasm32 uses Rust's, so a 600-tick session hash would
diverge on last-ulp differences (audio matched because it does no
transcendental math) — and a single-threaded tab would carry simulation,
animation, streaming and the renderer's rayon work on one thread.

A spike (`spike/worker-threads`, `825ad09`, headless Chrome 155 on Apple M4,
the NTSC dump) tested the alternative: the game on real threads inside Web
Workers, built with wasm atomics and shared memory. All five unknowns held:

1. **Threaded build**: core, audio, net and formats build with
   `+atomics,+bulk-memory,+mutable-globals`, shared memory and
   `-Z build-std` (nightly 2026-10-23).
2. **Threads**: `host_thread` gains a host spawner (a `WorkerHandle` over a
   `std` thread or a host-started one); with workers **on**, milestone 5's
   boot runs in a `wasm_thread` Web Worker, its readers start as 25 Web
   Workers blocking on real condvars, and the report matches native.
3. **WebGPU from a worker**: an `OffscreenCanvas` transferred to a module
   worker sharing the module and memory; wgpu renders continuously into the
   page; `texture-compression-bc` is available, 16384-texel textures.
4. **Desktop-style reads**: with no prefetch, core reads the archives through
   blocking ranged sync-XHR requests from its worker threads (22 for the
   boot) and the boot matches native.
5. **Shaders**: all 30 WGSL modules the native `--render-inspection` run
   creates compile in Chrome's Tint, after one behavior-preserving fix (the
   movie shader samples before its letterbox early return — uniformity).

So the game's own threads, condvars and synchronous reads keep working in a
worker; the main thread only forwards input, plays audio and never blocks.
Costs, accepted: the web crate builds on nightly (`-Z build-std`; native
stays on the pinned 1.99); the page must be cross-origin isolated (COOP/
COEP headers on every host); WebGPU only (WebGL2 cannot run the renderer:
multisampled depth/stencil reads, non-zero `base_vertex`).

Proof changes accordingly. Gates are native-equal where the math allows —
milestone 5's boot report, milestone 6's audio hash, the render-inspection
census counts — and otherwise observable: the game boots to free skate,
renders, takes input, holds a measured frame time. Bit-equality of a whole
session across hosts is not a gate.

## 1. Facts carried from the research and review (verified at `ba96d89`)

- Core spawn sites outside `host_thread`: the animation job pool
  (`animation_job_execution.rs:116-127`, three raw workers, Condvar
  waiters), recipe decode (`selected_living_world_recipes.rs:480-483`), the
  AI scene loader (`selected_session_ai.rs:617-620`), scene characters
  (`selected_session_scene_characters.rs:183-189`), career autosave
  (`selected_session_career.rs:270-279`), the remote-skater builder
  (`online/remote_skater.rs:534-545`); plus `skate2-net`'s threaded client
  and `skate2-audio`'s stream-pool readers. All become host-spawned; on a
  worker host none needs an inline mode.
- `std::time::Instant`/`SystemTime` trap on wasm32 even with atomics: the
  global RNG seed (`math/native_rw_math_random.rs:75-84`), `finish_initial_
  load`'s sleep is fine in a worker, core's online clocks
  (`selected_session_online.rs:368,1142`, `online_challenge.rs:1373,1383`,
  `lobby_browser.rs:60,78`), and the pc renderer/client clocks. `web_time`
  covers both types.
- File-system reads outside `MountSet`: `startup.ini` via `host_path`
  (`selected_original_assets_bootstrap.rs:96-98`) and the profile store
  (`edit_skater_profile.rs:141-155`, `current_dir` fallback); `OnlineConfig`
  already tolerates failure.
- The pc app is one binary crate (`main.rs` 14.7k lines declares ~70
  modules; the renderer ~23k lines). Its native-only edges: winit window and
  `pollster` (`render_window.rs`), SDL/XInput pads, cpal audio, the movie
  file reader, rfd dialogs, the updater, BMP/photo/replay file output, and a
  blocking capture readback (`gpu_renderer.rs:847-854`).
- The renderer writes to the session (`live_skating_client.rs`):
  `set_presentation_cloth` (:3956), `set_presentation_stream_area` (:3137),
  `set_replay_stream_focuses` (:3139), and the acknowledgements that gate
  simulation (`acknowledge_movie_screen_loaded`,
  `acknowledge_map_travel_presentation_ready`). A web host makes the same
  calls by running the same client code.
- WebGPU has no border clamp (the renderer falls back to `ClampToEdge`,
  `character_stamp.rs:167-172` etc.) and `Rg16Unorm` shadow formats need
  native-only features (fallback `Rg32Float`/`Rg16Float`): documented
  render deviations, recorded, not gates.
- The render-inspection census on this tree (Apple M4, Metal): 2,790 mesh
  uploads, 1,565 texture uploads, 2,286 retained materials, the environment
  shading draw map (`Base: 1739, TreeAnimate: 226, …`), `inspection diffuse
  draws=6`. A capture at a fixed eye/target is byte-stable across native
  runs.

## 2. Phases

### P1 — core: the host spawner, everywhere

From the spike, done properly: `host_thread::spawn_worker` keeps its
contract and gains the installed-spawner path (`WorkerHandle` with
`join`/`is_finished`; the name and stack size are passed through); every
spawn site in §1 routes through it (the animation pool and recipe decode
included — with no spawner installed they are exactly today's `std`
threads). `skate2-net`'s client thread and `skate2-audio`'s readers take a
spawner the same way. Gate: corpus tests and the boot-probe pair unchanged;
a unit test drives a fake spawner through join/is_finished/panic.

### P2 — core: clocks and the RNG seed on `web_time`

Every non-test `std::time::{Instant, SystemTime}` in core, audio and the
render/client code moves to `web_time` (the `std` types on native). Gate:
unchanged native behavior (corpus routes, audio oracle hash); the threaded
wasm build has no remaining `std::time` call on the boot path (a grep gate).

### P3 — host files: `startup.ini` and the profile

A host trait for the two loose reads: native keeps `host_path`/`SKATER.P`
exactly; the web host reads `startup.ini` by blocking HTTP and supplies an
absent profile (persistent saves via OPFS are later work). Gate: corpus
routes unchanged; a test that the absent profile never touches `current_dir`.

### P4 — the pc app becomes a library with a platform layer (native only)

`skate2-pc` gains a `lib.rs` holding today's modules; `main.rs` becomes the
thin native entry. The native-only edges (§1) move behind a `Platform`
seam: window and frame loop, pads, audio output, movie source, dialogs,
updater, file capture (async readback). `cfg` is allowed here (an app
crate). Gate: the pc corpus tests, the audio oracle, and the
render-inspection census and capture byte-identical before and after.

### P5 — the game worker (the milestone's core)

`apps/skate2-web` (nightly, threaded) runs `skate2-pc`'s live client in a
module worker over the transferred `OffscreenCanvas`, with the spawner
installed (no `disable_worker_threads`), archives mounted over blocking
ranged HTTP (prefetch kept as an optimization for the boot set), the web
`startup.ini`/profile host, and the movie shader fix. Input: the main
thread forwards keyboard and Gamepad state into a shared-memory slot the
worker's `PadPlatform` reads. Audio: the worker's mix feeds an
`AudioWorklet` through a `SharedArrayBuffer` ring (milestone six's
recorded follow-up). Proofs, in order:
1. `?inspect=1`: the original map renders in the tab; its printed census
   equals §1's native recording; a fixed-camera capture is compared with
   the native image and the difference recorded (backends differ; counts
   gate, pixels are evidence).
2. `?play=1`: boot to free skate with the scripted `perf-skate` pad; the
   panel reports ticks/s and frame time; a screenshot shows the skater.
3. Live play with keyboard/gamepad and audio — a hand check, recorded with
   frame-time figures.
Measure the per-thread stack first (the SIM thread asks 16 MiB;
wasm-bindgen's worker stacks are set at bind time) and the transfer volume
of a boot and of skating one district (blocking reads, so latency matters
off localhost; record both and the effect of prefetch).

### P6 — online through the bridge

Core's online gains a transport seam (the threaded UDP `Client` on native;
a WebSocket transport on the web host, driven from the game worker's event
loop) and injected connection parameters. The session's own frames,
recipe, facts and `Busy` replace milestone six's synthetic source. Gate:
`skate2-net`/bridge tests unchanged; the tab joins a native server and a
native pc client in the same lobby sees the tab's skater (hand check,
recorded with the server console).

### P7 — docs

`docs/WEB_PORT.md`: the worker route, its costs (nightly web toolchain,
COOP/COEP hosting, WebGPU only), the measured weights and frame times,
render deviations, the converted-pack option as future work, milestone
eight (persistence, AudioWorklet polish, shipping build) next.

## 3. Order

```
P1 ─► P2 ─► P3 ──┐
P4 ──────────────┼─► P5 ─► P6 ─► P7
```

P1–P3 (core) and P4 (the pc app) touch disjoint code and can run in
parallel worktrees; P5 needs all four.

## 4. Stop conditions

- A seam changes native behavior (a corpus route, the audio hash, the
  render census or capture): report; do not fork.
- The inspection census in the tab differs from native: report both.
- A per-thread stack or memory ceiling (4 GiB wasm32) is hit and cannot be
  raised: report the figure.
- Boot transfer past 512 MiB, or a district stream that stalls play off
  localhost: report the measurement; the owner decides (prefetch manifests,
  converted packs, or accept).
- Anything needs `cfg(target_arch)` in core or formats.
