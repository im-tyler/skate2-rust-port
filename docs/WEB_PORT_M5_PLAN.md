# Web port milestone 5: core streaming runs in the tab

Execution plan for milestone 5 of `docs/WEB_PORT.md`. Written to be carried out
phase by phase by an agent with no other context. Read sections 0–3 before
touching code. Every fact below was checked against the tree at `62cdce8`;
if the tree has moved and a cited line no longer matches, re-locate it by the
quoted code, not the line number.

---

## 0. Rules for the executor

1. **Read `AGENTS.md` first.** It is binding. The two rules this plan trips over
   most easily:
   - **Line endings are mixed (CRLF and LF, sometimes in one file). Keep each
     line's ending. Never normalize a file.** After every edit run
     `git diff --numstat` — a file showing hundreds of changed lines for a
     ten-line edit was normalized; revert it (`git checkout -- <file>`) and redo
     the edit.
   - Clippy `all` + `pedantic`, warnings denied. `unsafe` is forbidden.
2. **No `cfg(target_arch = ...)` in `crates/skate2-core` or
   `crates/skate2-formats`.** This plan is designed so the browser build needs
   none: wasm differences are expressed as host policy that also runs, and is
   tested, on native. If a step seems to need one, stop and report (section 7).
3. **Native behavior must not change** except where a phase says so explicitly.
   Every phase ends with the native gate green.
4. **One phase = one commit**, in order. Do not start phase N+1 until phase N's
   acceptance list is fully green. Push each green phase to `origin` (the
   `im-tyler` fork). **Never push to `upstream`.**
5. Do not "fix" a failing comparison by editing the expected values. Expected
   values come from a native run and are only ever recorded once, in the phase
   that says to record them.
6. Game data lives at `~/Documents/Skate2-360` (set
   `SKATE2_GAME_ROOT=$HOME/Documents/Skate2-360`). Nothing from it — bytes,
   extracted files, screenshots of assets — is ever committed. Offsets, sizes,
   paths and hashes are metadata and may be committed.
7. Preserve unrelated working-tree changes. No destructive git commands.

### Gates referenced by every phase

Native gate (repository root):

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Wasm gate (must run from `apps/skate2-web` — its `.cargo/config.toml` carries
the getrandom flag):

```sh
cd apps/skate2-web
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
trunk build --release
```

Corpus gate (ignored tests that need the dump):

```sh
SKATE2_GAME_ROOT=$HOME/Documents/Skate2-360 cargo test -p skate2-core --release -- --ignored <filter>
```

---

## 1. Goal and definition of done

**Goal:** the real core streaming machinery — `SelectedWorldStream` (the BAM
world stream to its finalized bounds) followed by `SelectedResidentStartup`
(one resident Simulation collection, `0x823c_4449_fb31_63b4`, the grind
corpus's collection) — runs to completion **inside the browser tab**, on the
player's own dump, single-threaded, and produces a report **identical to the
native run's**.

**Done means all of:**

- A native corpus test pins the probe's report (threaded, normal process).
- A second native corpus test, in its own process with worker threads
  disabled, produces the identical report. (This runs the exact code paths the
  browser takes, on native, under the normal test harness.)
- A native manifest tool proves the browser's prefetch list is closed: the
  probe completes against in-memory archive spans with zero misses and the
  identical report.
- The browser build runs the probe from the assets panel against `?http=` and
  shows `boot probe: done — matches native`, with no console errors.
- `docs/WEB_PORT.md` records it, with the evidence.

**Not in this milestone:** rendering any of it, booting a session, on-demand
streaming (fetching bytes the moment core asks), audio, networking. See
section 6.

---

## 2. Verified facts the design rests on

### 2.1 wasm32 runtime behavior (rustc 1.99, measured)

Spike: a cdylib built for `wasm32-unknown-unknown`, each function called from
Node via plain `WebAssembly.instantiate` (throwaway; rebuild it in minutes if
the toolchain changes). Results:

| Call | Result on wasm32 |
| --- | --- |
| `thread::Builder::new().spawn(..)` | returns `Err` (does not trap) |
| `thread::spawn(..)` | traps (it unwraps the `Err`) |
| `mpsc::channel` send + `try_recv` on one thread | works |
| `Condvar::notify_all` | works (no-op) |
| `Condvar::wait*` | **traps** |
| `Instant::now()` | **traps** |
| `SystemTime::now()` | **traps** |
| `thread::sleep` | **traps** |
| `std::env::var_os` | returns `None` |

Not measured but documented std behavior: `Mutex` on this target panics on a
re-entrant lock where native would deadlock; a blocking `mpsc::recv()` with no
other thread never returns.

Consequences: every env-gated profiling `Instant::now` in core is safe
(`var_os` is `None`); any **unconditional** `Instant::now`, `sleep`, or
`Condvar::wait` on the probe's path is fatal; every site that uses
`Builder::spawn(..).ok()` already degrades.

### 2.2 Core reads game bytes synchronously, everywhere

`skate2_formats::mount::MountSet` is a concrete type over files
(`crates/skate2-formats/src/mount.rs`: `MountedBig { data_path, header_path,
archive }`, every read does `File::open(data_path)` then `seek` + `read_exact`).
Core has ~97 non-test `read_path` call sites across ~58 files, plus the
collection producer's ranged reads (`impl CollectionLogicalRangeSource for
MountSet`, `crates/skate2-core/src/streaming/mounted_collection_producer.rs:43`).

The only seek/read shapes used on archive payloads are
`seek(SeekFrom::Start(_))` and `read_exact` (mount.rs ~456–597).

**This is the central constraint.** A browser cannot satisfy a synchronous
read from the network or from a picked `File`. So the browser must hold the
needed bytes **in memory before core asks**, behind the same synchronous
`MountSet` API. Async lives at the web edge (prefetch), not in core.

### 2.3 Unconditional clock read on every archive read

`MountedBig::read_entry` and `read_entry_range` call `Instant::now()` on
every read, only to report `elapsed` to the optional `observe_reads` observer.
On wasm32 that traps on the first byte core reads. The web app has never hit it
because steps 1–4 read through its own `RemoteMountSet`, not `MountedBig`.

### 2.4 Thread sites on the probe's path

Traced from `SelectedWorldStream::try_new`/`try_poll`
(`src/session/streaming/selected_world_stream.rs`) and
`SelectedResidentStartup::new`/`request`/`poll`
(`src/session/streaming/selected_resident_startup.rs`):

| Site | Today on wasm32 | Action |
| --- | --- | --- |
| `streaming/mounted_stream_backend.rs` `ReadScheduler::new` — 3 workers, `Condvar` | spawn `Err` → constructor returns `WorkerSpawn` error → **stream cannot start** | Phase 2: inline mode |
| `streaming/stream_reads.rs` `queue` — reader thread | spawn `Err` → reads run inline (by design) | Phase 2: route through host policy |
| `streaming/resident_collection_load_driver.rs:247` `Prefetch::spawn` | spawn `Err` → `None` → built at the drain (by design) | Phase 2: route through host policy |
| `physics/collision/static_ground_pool.rs:774` `warm_decode` decoder thread | spawn `Err` → no warm-up; first query decodes (by design) | Phase 2: route through host policy |
| `streaming/mounted_collection_producer.rs` background stage | uses `stream_reads::queue` + `mpsc::try_recv` → works inline | none |

All other `thread`/`Instant`/`sleep` sites in core are either test-only
(after the file's `#[cfg(test)]`), env-gated profiling, or off this path:
animation job pool (`animation/jobs/animation_job_execution.rs`), AI/scene/
living-world recipe loaders (incl. `finish_initial_load`'s `sleep` loop),
remote skater, career save, RNG seed (`math/native_rw_math_random.rs:78`,
`SystemTime`), `diagnostics::set_quiet`. These are milestone 7 (section 6).

### 2.5 The probe's owners are already poll-shaped

`SelectedWorldStream::try_poll() -> WorldStreamPoll::{Pending, CompletedThisPoll}`
and `SelectedResidentStartup::poll() -> bool`. The corpus drives them with
`while !done { yield_now() }`
(`physics/player/runtime/physical_player_camera_stream_corpus.rs:29`,
`physics/player/grind/physical_player_grind_corpus.rs:49`). A browser can drive
the same calls from a cooperative loop — no blocking loop in core is involved.

### 2.6 Corrections to the earlier draft plan

- "Synchronous wasm backend: `begin` reads inline" — **wrong as stated**: there
  is no synchronous byte source in a browser. Fixed by 2.2's resident spans.
- "wasm = traced mode" — **not used**. `traced_run()` also changes delivery
  timing (`exact_delivery`) and is env-driven; we change only threading.
- "RNG seed and `set_quiet` block everything before streaming" — they are not
  on this milestone's path; deferred.
- "Animation job pool is on the boot path" — not on this milestone's path.
- `mount.rs`'s per-read `Instant::now` (2.3) — **missed** by the draft; it is
  the first trap the browser would hit.
- "First platform `cfg` in core" — **avoided** entirely (rule 0.2).
- `docs/WEB_PORT.md` claims `owned_resident_collection.rs` spawns production
  workers — its spawns are test-only. Fix in phase 6.

---

## 3. Design

```
Browser tab (apps/skate2-web)                         Core (unchanged shape)
─────────────────────────────                         ──────────────────────
RemoteMountSet::open_all  (existing, async)
  │ tables
  ▼
boot-manifest.tsv  ── per entry: ranged fetch of the STORED span (async)
  │
  ▼
ResidentSpans (skate2-formats) ─ in-memory spans, sync read_at, records misses
  │
  ▼
MountedBig::with_storage(..)  ×20  ─►  MountSet  ─►  ResidentBootProbe::poll()
                                                      SelectedWorldStream
                                                      SelectedResidentStartup
host_thread::disable_worker_threads()  ─────────►    every optional worker
                                                      runs on the calling thread
cooperative loop: poll for ≤8 ms, yield (setTimeout 0), repeat
```

Four seams, all host policy, all exercised on native:

1. **Archive storage seam** (formats): `MountedBig` reads either its file
   (today, unchanged) or an `ArchiveRanges` implementation. The read functions
   are already generic over `Read + Seek`; a small cursor adapter feeds them.
2. **Worker policy** (core `host_thread`): a host can declare "no worker
   threads"; the four sites in 2.4 route their spawn through one helper that
   honors it. Native default: unchanged.
3. **Read scheduler inline mode** (core): when no workers can start, admitted
   reads run on the polling thread during `poll`, through the same
   `SchedulerState` (same 128-operation pool, same 3-active admission, same
   FIFO, same lease release).
4. **A public boot probe** (core): one pub type driving the two owners above,
   returning a comparable report.

The manifest is generated natively by recording every archive entry the probe
reads (`skate2_formats::mount::observe_reads` already exists for this), keyed
by archive + entry key (name or hash) — **not by offset**, so it stays valid
across dumps whose offsets differ (this NTSC dump's `skater.vlt` already
differs in size from the corpus researcher's).

---

## 4. Phases

### Phase 1 — formats: archive storage seam and a lazy clock

Files: `crates/skate2-formats/src/mount.rs`, new
`crates/skate2-formats/src/resident_spans.rs`, `crates/skate2-formats/src/lib.rs`.

1. **Lazy clock.** In `MountedBig::read_entry` and `read_entry_range`, take
   `Instant::now()` only when an observer is installed:
   `let started = READ_OBSERVER.get().map(|_| Instant::now());` and make
   `observe` take `Option<Instant>`, returning early on `None`. Observer output
   is unchanged when one is installed.
2. **Storage seam.**
   ```rust
   /// Random-access bytes of one archive's data file, for hosts that do not
   /// read archives from a file system (the browser build).
   pub trait ArchiveRanges: Send + Sync + fmt::Debug {
       /// Fills `buffer` with the bytes at `offset`, entirely or not at all.
       fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;
   }
   ```
   Add a private `enum ArchiveStorage { File, Ranges(Arc<dyn ArchiveRanges>) }`
   field `storage` on `MountedBig` (keep `#[derive(Clone, Debug)]` working).
   `MountedBig::open` sets `File` — native path byte-identical.
   Add:
   ```rust
   /// An archive whose table was read by the host and whose payload is served
   /// by `ranges`; `data_path`/`header_path` are names only, never opened.
   pub fn with_storage(data_path: PathBuf, header_path: PathBuf,
                       archive: BigArchive, ranges: Arc<dyn ArchiveRanges>) -> Self
   ```
   In `read_entry`/`read_entry_range`, replace `open_file(&self.data_path)?`
   with a match: `File` → the existing `open_file`; `Ranges` → a private
   `RangesCursor { ranges, position }` implementing `Read` (via `read_at`,
   returning `buffer.len()` on success) and `Seek` (`SeekFrom::Start` only;
   other variants → `io::ErrorKind::Unsupported`). Map errors exactly as the
   file path does (`MountError::Io { path: data_path, .. }`).
3. **Raw span read**, needed by the manifest tool:
   ```rust
   /// The entry's stored bytes as they sit in the data file: no RefPack
   /// expansion, no size validation.
   pub fn read_stored(&self, entry: &BigEntry) -> Result<Vec<u8>, MountError>
   ```
4. **`ResidentSpans`** (`resident_spans.rs`, `pub mod resident_spans`):
   in-memory stored spans of one archive, keyed by start offset
   (`Mutex<BTreeMap<u64, Arc<[u8]>>>`), plus a shared miss log.
   - `pub fn insert(&self, offset: u64, stored: Vec<u8>)`
   - `read_at`: find the span with the greatest start `<= offset`; if
     `offset + len` fits inside it, copy; otherwise record
     `(archive name, offset, len)` in the miss log and return
     `io::Error::new(io::ErrorKind::NotFound, "not resident: <name> <offset>+<len>")`.
   - `pub fn misses(&self) -> Vec<(u64, usize)>`, `pub fn resident_bytes(&self) -> u64`.
   - Never lock while calling out (no callbacks exist; keep it that way).
5. Commit the pending one-line `Cargo.lock` change (`skate2-web` gained its
   `skate2-formats` dependency in step one) in this phase.

Tests (in `mount.rs`'s existing test module / `resident_spans.rs`):
- An archive built in a temp file (the module already has `TestFile`) and the
  same archive as `with_storage` over `ResidentSpans` holding every entry's
  stored span: `read_path`, `read_entry`, `read_path_range` (raw and RefPack
  entries, if the existing fixtures cover RefPack) return identical results.
- A missing span → `MountError::Io` with `NotFound`, and the miss is logged.
- A read straddling two adjacent spans → miss (spans are never stitched).

Acceptance: native gate; wasm gate; the existing mount corpus tests
(`cargo test -p skate2-formats --release -- --ignored` with
`SKATE2_GAME_ROOT`) still pass.

Commit: `Web port step five (a): archive storage seam and lazy read clock`.

### Phase 2 — core: worker policy and the scheduler's inline mode

Files: `crates/skate2-core/src/host_thread.rs`,
`src/streaming/stream_reads.rs`, `src/streaming/resident_collection_load_driver.rs`,
`src/physics/collision/static_ground_pool.rs`,
`src/streaming/mounted_stream_backend.rs`.

1. **Host policy** in `host_thread.rs` (matches the module's existing role:
   "lets a platform describe its threads"):
   ```rust
   /// The host cannot run worker threads (the browser build). Optional workers
   /// then do their work on the calling thread. Call once, before any game work:
   /// workers already started, and lazily started readers, are not affected.
   pub fn disable_worker_threads()

   pub(crate) fn worker_threads_available() -> bool

   /// `builder.spawn(work)`, or `Unsupported` when the host disabled workers.
   pub(crate) fn spawn_worker<F, T>(builder: std::thread::Builder, work: F)
       -> std::io::Result<std::thread::JoinHandle<T>>
   where F: FnOnce() -> T + Send + 'static, T: Send + 'static
   ```
   Backed by a `static AtomicBool`.
2. Route exactly three existing `Builder::spawn` calls through `spawn_worker`,
   changing nothing else: `stream_reads::queue`, `Prefetch::spawn`,
   `static_ground_pool::warm_decode`. Their existing `.ok()` fallbacks then
   engage. **Do not route any other spawn site** (animation pool, AI, scene
   characters, living-world recipes, remote skater, career save) — they have no
   fallback and are milestone 7.
3. **`ReadScheduler` inline mode** (`mounted_stream_backend.rs`):
   - Extract the post-read block of `worker_main` (from `let outcome = ...`
     through the final `notify_all`) into
     `fn record_outcome(core: &SchedulerCore, id: u64, outcome: thread::Result<SourceRead>)`
     — check the real type of `id` and `outcome`. `worker_main` calls it.
     Pure extraction: native behavior identical.
   - Add `inline: bool` to `ReadScheduler`. A private `fn inline(source) -> Self`
     builds the core with no workers.
   - `ReadScheduler::new`: if `!host_thread::worker_threads_available()` →
     `Ok(Self::inline(source))`. Spawn workers through `spawn_worker`; on any
     spawn failure, run the **existing** shutdown-and-join of already-started
     workers, then return `Ok(Self::inline(source))` with a fresh core. The
     pool is all-or-nothing: never a partial pool.
   - `fn run_ready_inline(&self)`: loop — lock; pop `ready` front (skip ids no
     longer in `operations`, as the worker does); set `Reading`; clone the
     request; **release the lock**; `catch_unwind(|| source.read(&request))`;
     `record_outcome(..)`; until `ready` is empty or a fault is set.
   - `poll`: when `inline`, call `run_ready_inline()` first, then the existing
     body. **`begin` performs no I/O in either mode** (the native contract).
   - `Drop`: unchanged (zero workers to join).
   - `MountedStreamBackendError::WorkerSpawn` is no longer produced. If it is
     referenced only inside this file, remove it and update the `new`/
     `with_source` docs; otherwise keep it and say so in the commit.
   - Update the module doc: inline mode is PC/host policy, same pool, same
     admission, same FIFO, same lease release; reads happen at `poll`.
4. Tests, in the existing test module, constructing `ReadScheduler::inline`
   directly (no global flag — tests run in parallel):
   - FIFO: begin 3, poll in reverse order — all `Complete` with the right bytes.
   - Admission: begin 4; the 4th stays `Pending` across polls until one of the
     first three handles is dropped; then it completes.
   - Capacity: the 129th `begin` → `OperationPoolExhausted`.
   - Dropping a `Pending` handle discards it without a read (source records
     reads; assert it was never read).
   - Source returns `Failed` → `AsyncReadPoll::Failed`.
   - Source panics → scheduler fault `WorkerPanicked` on the next call.
   - Mirror any other existing scheduler test that does not depend on a source
     that blocks; list the ones you skipped and why in the commit body.
   - `host_thread`: `spawn_worker` refuses after `disable_worker_threads()` —
     **put this test in its own integration test file**
     (`crates/skate2-core/tests/host_thread_policy.rs`), because the flag is
     process-global.

Acceptance: native gate; wasm gate; existing `mounted_stream_backend` tests all
pass unchanged; corpus gate for the stream: run the existing ignored tests that
drive `camera_stream::load` and `grind_corpus::mount` (find them with
`grep -rln 'grind_corpus::mount\|camera_stream::load' crates/skate2-core/src`;
run by their test names) — still green.

Commit: `Web port step five (b): worker policy and the read scheduler's inline mode`.

### Phase 3 — core: the public resident boot probe

Files: new `crates/skate2-core/src/session/streaming/resident_boot_probe.rs`
registered in `src/lib.rs` as `pub mod resident_boot_probe` (follow the
`#[path = ...]` pattern used there); new integration tests
`crates/skate2-core/tests/boot_probe_threaded.rs`,
`crates/skate2-core/tests/boot_probe_threadless.rs`,
`crates/skate2-core/tests/common/mod.rs`.

1. API (adjust field types to what the owners actually expose; keep names):
   ```rust
   /// The grind corpus's resident collection in the BAM world stream.
   pub const BOOT_PROBE_COLLECTION: u64 = 0x823c_4449_fb31_63b4;

   #[derive(Clone, Debug, PartialEq, Eq)]
   pub struct ResidentBootReport {
       pub high_collections: u32,      // WorldStreamEffects::add_high_collection calls
       pub bounds_min: [u32; 3],       // finalized bounds, raw f32 bits
       pub bounds_max: [u32; 3],
       pub resident_plans: usize,      // SelectedResidentStartup::plans().len()
       pub retained_counts: (usize, usize), // assets().retained_counts()
   }

   pub enum ResidentBootPoll { Streaming, Resident, Done(ResidentBootReport) }

   pub struct ResidentBootProbe { /* stream, resident, effects counters, poll counts */ }

   impl ResidentBootProbe {
       pub fn new(mounts: Arc<MountSet>) -> Result<Self, String>;
       /// One poll of whichever owner is current; never blocks, never sleeps.
       pub fn poll(&mut self) -> Result<ResidentBootPoll, String>;
       /// (stream polls, resident polls) — host diagnostics, not part of the report.
       pub fn polls(&self) -> (u32, u32);
   }
   ```
2. Build it from the corpus recipe, minus asserts and deadlines:
   `SelectedWorldStream::try_new(mounts, WorldStreamCatalog::new(), cvars,
   WorldStreamSpec::derive("BAM", None)?, effects)`; poll to
   `CompletedThisPoll`; read bounds; then
   `SelectedResidentStartup::new(mounts, stream.catalog.clone())`,
   `request(CollectionId(BOOT_PROBE_COLLECTION))`, poll to `true`. Keep the
   stream alive for the probe's lifetime (the corpus retains it too).
   - `effects`: a small private `WorldStreamEffects` impl that counts
     `add_high_collection` and records `finalize_world_bounds`.
   - `cvars`: use the constructor the corpus callers use
     (`InputCvarReferences::new_static_initialized()` is `pub(crate)` and not
     test-gated — confirm). If the stream needs cvars the corpus links from
     `input.cfg`, do the same linking. If that turns out to need test-only
     code, stop and report.
   - Errors become `String` via `format!("{error:?}")`; no `unwrap`/`expect`
     outside tests.
3. `tests/common/mod.rs`: `pub fn game_mounts() -> Arc<MountSet>` (loop over
   `skate2_formats::mount::SKATE2_ARCHIVES`, `path_under(root)`, skip missing,
   `MountedBig::open(path, spec.use_external_header)`) and
   `pub fn run_probe(mounts) -> ResidentBootReport` (poll until `Done`, with a
   generous wall-clock deadline — tests are native) and `pub const EXPECTED:
   ResidentBootReport`.
4. **Record the oracle once:** run the threaded test with `EXPECTED` set to a
   placeholder, take the printed report, write it into `EXPECTED`. Put the raw
   printed output in the commit body. This is the only time expected values
   are written.
5. `boot_probe_threaded.rs`: `#[ignore = "requires external SKATE2_GAME_ROOT"]`,
   asserts `run_probe(game_mounts()) == EXPECTED`.
   `boot_probe_threadless.rs`: same, but calls
   `skate2_core::host_thread::disable_worker_threads()` first. Separate file =
   separate process; that is what makes the global flag safe.

Acceptance: native gate; wasm gate; both corpus tests pass
(`cargo test -p skate2-core --release --test boot_probe_threaded -- --ignored`,
same for `threadless`). **If threaded and threadless disagree, stop** — that is
a real ordering bug in the inline paths; report both outputs (section 7).

Commit: `Web port step five (c): the resident boot probe, pinned on native threaded and threadless`.

### Phase 4 — the boot manifest and its closure proof (native)

Files: new `apps/skate2-web/src/bin/boot_manifest.rs`, new
`apps/skate2-web/boot-manifest.tsv` (generated, committed),
`apps/skate2-web/Cargo.toml` only if a `[[bin]]` entry is required.

The binary is native-only: wrap its body in
`#[cfg(not(target_arch = "wasm32"))]` with a no-op `main` otherwise
(this is the web app crate, not core — rule 0.2 does not apply here). Confirm
`trunk build --release` still bundles only the `skate2_web` cdylib.

1. `disable_worker_threads()` first (the browser's configuration).
2. **Record:** install `observe_reads` with an observer that inserts
   `(archive data_path, entry.key, entry.stored_size)` into a
   `static Mutex<BTreeSet<..>>`. Run the probe on file-backed mounts
   (same loop as `tests/common`). Keep its report as `file_report`.
3. **Close:** for each mounted archive, a `ResidentSpans` holding, for every
   recorded entry of that archive, `read_stored(entry)` at `entry.offset`.
   Build a second `MountSet` with `MountedBig::with_storage(data_path,
   header_path, archive.clone(), spans)` in the same order. Run a fresh probe on
   it → `span_report`.
4. Pass condition: zero misses across all `ResidentSpans` **and**
   `span_report == file_report`. On misses, print them and exit non-zero
   without writing the manifest. (Misses here mean a read path bypasses the
   observer — investigate; do not paper over by adding entries by hand.)
5. Write `boot-manifest.tsv`, deterministic order (mount index, then offset):
   ```
   # skate2 boot manifest v1
   # report	<Debug of ResidentBootReport on one line>
   # entries	<count>	stored_bytes	<sum>
   <mount path as in SKATE2_ARCHIVES>	name	<virtual path>	<stored_size>
   <mount path>	hash	<0x%08x>	<stored_size>
   ```
   Keyed by entry key, not offset. Print entry count and total MiB.
6. **Size check:** if total stored bytes exceed 1 GiB, stop and report before
   phase 5 (section 7). Record the actual figure in the commit body either way.

Acceptance: native gate; wasm gate; the tool exits 0 and the TSV's `# report`
equals `EXPECTED` from phase 3.

Commit: `Web port step five (d): boot manifest, closed against in-memory spans`.

### Phase 5 — the probe in the browser

Files: `apps/skate2-web/src/game_files.rs` (or a new `boot_probe.rs` module),
`apps/skate2-web/src/lib.rs`, `apps/skate2-web/index.html`.

1. At wasm start, before anything else touches core:
   `skate2_core::host_thread::disable_worker_threads()`.
2. `include_str!("../boot-manifest.tsv")`; parse the header report line and
   entries (unit-test the parser natively).
3. After `RemoteMountSet::open_all`, for each manifest entry: find the mounted
   archive by mount path, resolve the entry (`find_path` for names,
   `find_hash` for hashes; if a key resolves to nothing, that is a dump
   mismatch — report it by name, do not skip silently), fetch
   `entry.offset..entry.offset + entry.stored_size` with the existing
   `ByteSource::read_range`, insert into that archive's `ResidentSpans`.
   Show progress (entries, MiB) in the assets panel. Sequential fetches are
   acceptable for this milestone; measure the time.
4. Build the core `MountSet` from `RemoteMountSet`'s archives via
   `MountedBig::with_storage` (`data_path` = the mount path, `header_path` =
   the `.bh` path when the table came from it). Add a native unit test that
   `MOUNT_PATHS` lists the same paths in the same order as
   `SKATE2_ARCHIVES` — mount order decides which archive serves a path.
5. Drive the probe **without `requestAnimationFrame`** (rAF does not tick in
   background or headless tabs): a `wasm_bindgen_futures::spawn_local` loop
   that polls repeatedly until 8 ms have passed (the crate's existing
   `Performance` clock), then yields with a `setTimeout(0)` promise. Track
   total time and the longest single `poll`.
6. Panel lines: `boot probe: streaming… / resident… (polls a/b)`, then either
   `boot probe: done — matches native` (report == manifest's `# report`) or
   `boot probe: MISMATCH` with both reports, plus total time, longest poll,
   resident MiB, and any `ResidentSpans` misses.
7. Trigger: a "Run boot probe" button in the assets panel, and automatically
   when the URL has `?boot=1`.

Acceptance: native gate; wasm gate; `cargo test -p skate2-web`; then the
browser check: `trunk build --release`, serve `dist/` and the dump the same way
steps 2–4 were verified (a Range-aware static server for the dump, CORS
allowed), open `…/?http=<dump base>&boot=1` in Chrome, wait for the panel.
Pass = `done — matches native`, zero misses, no console errors. Capture the
panel text and timings for the commit body and WEB_PORT.md.

Commit: `Web port step five (e): core's world stream and a resident collection loaded in the tab`.

### Phase 6 — docs

`docs/WEB_PORT.md`:
- Status table row and road table row 5 → done, renamed
  "Core streaming in the tab (single-threaded, prefetched)".
- Insert the new milestone between 5 and 6: **"On-demand streaming: a
  resident-spans miss becomes a pending read instead of an error"** (see 6).
- Blockers section: replace the threads paragraph with what was actually done
  and what remains; correct the `owned_resident_collection.rs` claim
  (test-only spawns); add the `mount.rs` clock finding.
- Verification paragraph: the browser run's panel text, timings, manifest size.
- Mark this plan file as executed (one line at the top) — do not delete it.

Acceptance: native gate. Commit: `Web port step five: docs`.

---

## 5. Order and dependencies

```
1 formats seam ──► 2 worker policy + inline scheduler ──► 3 probe (oracle) ──► 4 manifest ──► 5 browser ──► 6 docs
```

Strictly sequential. Phase 3's two native tests are the most important
evidence in the milestone: they prove the browser's threading configuration is
behaviorally identical on native before any browser is involved.

---

## 6. Deliberately out of scope (and where it goes)

| Item | Why not now | Lands in |
| --- | --- | --- |
| On-demand streaming (fetch on a miss) | Core's read sites latch failures (`SelectedResidentStartup.failed`, producer `terminal_error`); a miss must become `Pending`, starting at the already poll-shaped `ReadScheduler`. Needs its own design. | new milestone after 5 |
| Animation job pool, AI/scene/living-world loaders, `finish_initial_load` sleep loop, remote skater, career save thread | No inline fallback; not on this path; session boot needs them | 7 |
| RNG seed `SystemTime::now` (`native_rw_math_random.rs:78`), `diagnostics::set_quiet`/`start` `Instant` | Not on this path; needs a host clock/seed hook | 7 |
| Web's duplicated RefPack/size checks (`read_remote_entry`) and `MOUNT_PATHS` | Can be retired once everything reads through `MountSet` | cleanup after 5 |
| True threads (`SharedArrayBuffer`, COOP/COEP) | Not needed for correctness; only for speed | later, if profiling demands |

---

## 7. Stop conditions — stop, report, do not improvise

- Any step would need `cfg(target_arch)` in core or formats.
- Threaded and threadless reports differ (phase 3).
- The manifest closure has misses that the observer cannot explain (phase 4).
- Manifest total exceeds 1 GiB (phase 4).
- Any pre-existing native or corpus test changes result.
- A wasm trap/panic in the browser whose source is not one of the sites in 2.4
  (report the console stack; that site needs the same seam treatment, and the
  owner decides whether it belongs in this milestone).
- `Mutex` "cannot recursively acquire" panic in the browser: an inline path
  runs work while its caller holds a lock. Report the stack; the fix is to
  release the lock before the inline call, but confirm with the owner first.

## 8. Report format after each phase

```
Phase N — <title>
Commit: <sha> (pushed to origin: yes/no)
Changed: <files, one line each, with +/- counts>
Gates: fmt pass/fail · test pass/fail (<n> passed) · clippy native pass/fail · clippy wasm32 pass/fail · trunk pass/fail · corpus <names> pass/fail
Evidence: <the numbers this phase produced>
Deviations from the plan: <none | what and why>
Open questions: <none | ...>
```
