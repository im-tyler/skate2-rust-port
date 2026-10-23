# Web port milestone 5a: on-demand streaming — a miss becomes a pending read

Execution plan for milestone 5a of `docs/WEB_PORT.md`. Same rules as
`docs/WEB_PORT_M5_PLAN.md` section 0 (AGENTS.md binding; line endings; one
phase = one commit; gates scoped to touched crates on this machine; never
edit recorded expected values; stop, report, do not improvise). Facts below
were verified against `c0b197f`.

## 0. Design

Core reads synchronously everywhere; today a span miss is terminal
(`NotFound` → `MountError::Io` → latched failure in whichever owner). The
design changes the miss's *kind*, not core's shape:

1. **`ResidentSpans::read_at` misses answer `ErrorKind::WouldBlock`**
   ("not ready, ask again") instead of `NotFound`, and the span set carries
   an optional **miss handler** the host installs — called before the error
   returns, so the host can kick an async fetch that fills the span.
2. **The read scheduler** (`mounted_stream_backend`) gains
   `SourceRead::Retry`: `MountReadSource` maps a `WouldBlock` `MountError`
   to it, and `record_outcome` re-queues the operation (front of `ready`,
   phase back to `Admitted`, `thread::yield_now` guard for worker mode).
   `poll` then reports `Pending` until the span arrives — exactly what
   `AsyncReadPoll::Pending` was for.
3. **The collection producer** (`mounted_collection_producer`) maps a
   `WouldBlock` source error to its existing `Reading` poll instead of
   latching: `read_stage` is idempotent (range recomputed from
   `read_cursor`), and a failed background stage re-queues the same range.
4. **Bootstrap stays prefetched**: the boot's direct synchronous reads
   (Vault, catalog — outside both paths) use the committed manifest's 12
   spans exactly as milestone five did. On-demand covers everything the
   boot *streams*: the world-stream scheduler and the resident producer.
5. **Native is untouched in behavior**: files never answer `WouldBlock`,
   so `Retry` and the producer's not-latched branch are unreachable on
   native — the same browser-policy pattern milestone five used, tested on
   native by constructing the miss directly.

The proof: boot from the manifest (as milestone five), then request a
**second resident collection that is not in the manifest**. Its reads miss,
the handler fetches them over the network on demand, `Retry`/`Reading`
hold the line, and the load completes — on native (with a blocking fetch
in the miss handler) and in the browser (with an async one).

## 1. Phases

### Phase 1 — formats: `WouldBlock` misses and the miss handler

Files: `crates/skate2-formats/src/resident_spans.rs`, its tests in
`mount.rs`.

- `ResidentSpans::new` keeps its name; add
  `with_miss_handler(handler: Box<dyn Fn(u64, usize) + Send + Sync>)`
  (builder-style; the handler is called with the miss's offset and length
  before the error returns; handlers must not panic or lock the spans).
- Miss errors become `ErrorKind::WouldBlock` with the same
  `not resident: …` message. Update phase one's tests (`NotFound` →
  `WouldBlock`); the manifest tool needs no change — any miss still fails
  its closure proof.
- Gate: formats tests; corpus mount tests; wasm clippy.

Commit: `Web port step 5a (a): span misses answer WouldBlock, with a host miss handler`.

### Phase 2 — core: the scheduler's `Retry`

Files: `crates/skate2-core/src/streaming/mounted_stream_backend.rs`.

- `enum SourceRead { Complete(Vec<u8>), Failed, Retry }` — `Retry` means
  "the source is not ready; re-attempt this operation on a later poll".
- `MountReadSource::read`: a `MountError::Io` whose `source.kind()` is
  `WouldBlock` maps to `Retry`; every other error stays `Failed`.
- `record_outcome`: on `Ok(SourceRead::Retry)` set the operation's phase
  back to `Admitted`, push its id to the **front** of `ready` (FIFO
  preserved), `notify_all`, and `thread::yield_now()` as a worker-mode
  spin guard (unreachable in production: only span misses produce
  `Retry`, and spans only exist in the browser's inline mode).
- Tests: inline mode — a source that `Retry`s twice then completes (the
  poll stays `Pending`, then `Complete` with the right bytes); a `Retry`
  after the handle dropped discards cleanly; admission is not consumed by
  a `Retry` (the 4th begin still waits for a real completion's release).

Commit: `Web port step five a (b): the read scheduler retries a not-ready source`.

### Phase 3 — core: the producer's not-latched miss

Files: `crates/skate2-core/src/streaming/mounted_collection_producer.rs`.

- `read_stage`: a `WouldBlock` source error returns
  `Ok(MountedCollectionProducerPoll::Reading)` without latching (the range
  is recomputed from `read_cursor` next poll; nothing advanced).
- `take_background_stage` on a `WouldBlock` error: re-queue the same stage
  (`queue_background_stage(Some(logical_len))`) and report `Reading`,
  without latching.
- A `WouldBlock` test-source exercise: poll returns `Reading` repeatedly,
  then completes once the source stops blocking; no `terminal_error`.
- Gate: producer tests; the driver's corpus pair (threaded/threadless)
  still identical.

Commit: `Web port step five a (c): the collection producer treats a not-ready read as Reading`.

### Phase 4 — core: the probe's second collection, pinned on native

Files: `crates/skate2-core/src/session/streaming/resident_boot_probe.rs`,
`crates/skate2-core/tests/common/mod.rs`.

- `ResidentBootReport` gains `second_plans: usize` and
  `second_retained: (usize, usize)`; `ResidentBootPoll` gains
  `SecondResident` between `Resident` and `Done`.
- `ResidentBootProbe::request_more()` (pub): after `Done`'s first report
  fields are captured, requests a second, *unprefetched* collection —
  picked as another catalog collection the manifest does not name (the
  grind corpus's sibling district; find one by loading the catalog's
  collection ids in the probe's `new` and choosing deterministically the
  first whose id differs from `BOOT_PROBE_COLLECTION` and whose asset
  count is nonzero). The driver polls to `Done` for the second report.
- Record the extended oracle once (threaded), assert threadless equals it
  (same two-test shape as phase 3 of milestone five). With phase-1 spans
  and a *blocking* miss handler installed by the tests' mount harness
  (fetch = `read_stored` from the file mounts), the second collection
  loads through Retry/Reading on native — that is the on-demand proof
  before any browser.
- The manifest tool re-runs and stays 12 entries: the second collection
  is deliberately absent (bootstrap-only manifest; note the count check
  now compares against `# entries` of the committed file, unchanged).

Commit: `Web port step five a (d): a second, unprefetched collection loads on demand, pinned natively`.

### Phase 5 — the browser: on demand in the tab

Files: `apps/skate2-web/src/boot_probe.rs`, `src/lib.rs`.

- The spans are constructed with the miss handler: `spawn_local` a ranged
  fetch of the missed span into the set (the handler receives offset and
  length; the archive's data path is known per span set). Count on-demand
  fetches and bytes for the panel.
- The driver: prefetch the manifest's 12 spans (bootstrap), drive the
  probe to `Done`, call `request_more()`, drive to the extended `Done`.
- Panel: the existing line, plus `on-demand: N spans (X KiB) fetched`;
  pass = `done — matches native` including the second collection's
  fields, zero latched failures.
- Gate: headless browser run with `?boot=1` (the second collection's
  fetches must appear in the served-dump access log; the closure of the
  *manifest* is intentionally not re-checked — misses are the point).

Commit: `Web port step five a (e): the second collection streams on demand in the tab`.

### Phase 6 — docs

`docs/WEB_PORT.md`: 5a done (both code paths, the native pin, the browser
line); the on-demand paragraph in the asset section; the manifest's role
narrowed to bootstrap; milestone 6 next.

Commit: `Web port step five a: docs`.

## 2. Stop conditions

- Any phase needs `cfg(target_arch)` in core or formats.
- `Retry`/`Reading` changes a native observable (a corpus test result
  differs, or the threaded/threadless pair disagrees).
- The second collection's on-demand load latches a failure on native with
  the blocking handler — that is a real gap in the design; report the
  latch site.
- A miss handler re-entrancy hazard appears (handler locking the span set
  it was called from): the contract is documented in phase 1; violations
  are a stop, not a patch.
