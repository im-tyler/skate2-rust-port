//! Shared harness for the resident boot probe corpus tests
//! (`docs/WEB_PORT_M5_PLAN.md`, phase 3; `docs/WEB_PORT_M5A_PLAN.md`,
//! phase 4). The two test binaries differ only in
//! `disable_worker_threads()`; everything else — including the expected
//! report — is byte-identical here.

#![allow(dead_code)]

use skate2_core::resident_boot_probe::{ResidentBootPoll, ResidentBootProbe, ResidentBootReport};
use skate2_formats::{
    big::BigEntryKey,
    mount::{ArchiveRanges, ArchiveRead, MountSet, MountedBig, SKATE2_ARCHIVES, observe_reads},
    resident_spans::ResidentSpans,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// The game dump, from the same environment variable every corpus test uses.
fn game_root() -> PathBuf {
    PathBuf::from(
        std::env::var_os("SKATE2_GAME_ROOT").expect("SKATE2_GAME_ROOT names the game dump"),
    )
}

/// All archives the dump holds, in mount order, exactly as the game mounts.
pub fn game_mounts() -> Arc<MountSet> {
    let root = game_root();
    let mut mounts = MountSet::new();
    for spec in SKATE2_ARCHIVES {
        let path = spec.path_under(&root);
        if path.is_file() {
            mounts.push(
                MountedBig::open(path, spec.use_external_header).expect("the dump's archive opens"),
            );
        }
    }
    Arc::new(mounts)
}

/// One recorded entry read: which archive, a bijective display of the entry
/// key (`n<name>` / `h<hash>`), and where it sits. Display-keyed because
/// `BigEntryKey` carries no ordering.
type ReadKey = (PathBuf, String, u64, u64);

/// The bijective display form of an entry key.
fn display_of(key: &BigEntryKey) -> String {
    match key {
        BigEntryKey::Name(name) => format!("n{name}"),
        BigEntryKey::Hash(hash) => format!("h{hash:010x}"),
    }
}

static SEEN: OnceLock<Mutex<BTreeSet<ReadKey>>> = OnceLock::new();

/// The read observer: records every entry the probe reads, on whichever
/// thread the read runs.
fn record(read: &ArchiveRead<'_>) {
    let seen = SEEN.get_or_init(|| Mutex::new(BTreeSet::new()));
    if let Ok(mut seen) = seen.lock() {
        seen.insert((
            read.archive.to_owned(),
            display_of(&read.entry.key),
            read.entry.offset,
            read.entry.stored_size,
        ));
    }
}

fn recorded_reads() -> BTreeSet<ReadKey> {
    SEEN.get()
        .and_then(|seen| seen.lock().ok().map(|seen| seen.clone()))
        .expect("the observer recorded reads")
}

/// Runs the probe to `Done` with a generous native wall-clock deadline.
pub fn run_probe(mounts: Arc<MountSet>) -> ResidentBootReport {
    let mut probe = ResidentBootProbe::new(mounts).expect("the boot probe starts");
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        match probe.poll().expect("the boot probe polls") {
            ResidentBootPoll::Done(report) => {
                eprintln!(
                    "boot probe: done in {} stream polls, {} resident polls: {report:?}",
                    probe.polls().0,
                    probe.polls().1
                );
                return report;
            }
            _ => {
                assert!(Instant::now() < deadline, "boot probe timed out");
                std::thread::yield_now();
            }
        }
    }
}

/// Span-backed mounts for the on-demand proof: every archive mounted over
/// [`ResidentSpans`] holding exactly the bootstrap read set, plus a blocking
/// miss handler that fetches any other span from the file-backed archive —
/// the native stand-in for the browser's async fetch. Returns the mount set
/// and its span sets (for the miss count the proof asserts).
fn ondemand_span_mounts(
    root: &Path,
    bootstrap: &BTreeSet<ReadKey>,
) -> (Arc<MountSet>, Vec<Arc<ResidentSpans>>) {
    let mut mounts = MountSet::new();
    let mut span_sets = Vec::new();
    for spec in SKATE2_ARCHIVES {
        let data_path = spec.path_under(root);
        if !data_path.is_file() {
            continue;
        }
        let files = Arc::new(
            MountedBig::open(&data_path, spec.use_external_header)
                .expect("the dump's archive opens for spans"),
        );
        // The handler and the span set reference each other; the spans Arc
        // is published to the handler's slot right after construction, before
        // any read can miss.
        let spans_slot = Arc::new(OnceLock::new());
        let spans = Arc::new(
            ResidentSpans::new(spec.relative_path)
                .with_miss_handler(blocking_fetch(Arc::clone(&files), Arc::clone(&spans_slot))),
        );
        let _ = spans_slot.set(Arc::clone(&spans));
        let archive = files.archive().clone();
        for (archive_path, display, offset, _) in bootstrap {
            if *archive_path != data_path {
                continue;
            }
            let entry = archive
                .entries
                .iter()
                .find(|entry| display_of(&entry.key) == *display)
                .expect("a bootstrap entry is absent on remount");
            spans.insert(
                *offset,
                files.read_stored(entry).expect("the bootstrap span reads"),
            );
        }
        mounts.push(MountedBig::with_storage(
            files.data_path().to_owned(),
            files.header_path().to_owned(),
            archive,
            Arc::clone(&spans) as Arc<dyn ArchiveRanges>,
        ));
        span_sets.push(spans);
    }
    (Arc::new(mounts), span_sets)
}

/// The blocking miss handler: fetches the stored span covering the missed
/// offset from the file-backed archive and makes it resident before the
/// `WouldBlock` answer returns, so the scheduler's re-read succeeds. The
/// handler never reads the span set it serves — only inserts, outside the
/// span lock it was called from.
fn blocking_fetch(
    files: Arc<MountedBig>,
    spans: Arc<OnceLock<Arc<ResidentSpans>>>,
) -> Box<dyn Fn(u64, usize) + Send + Sync> {
    Box::new(move |offset, _length| {
        let Some(spans) = spans.get() else {
            return;
        };
        let entry = files.archive().entries.iter().find(|entry| {
            offset >= entry.offset && offset - entry.offset < u64::from(entry.stored_size)
        });
        let Some(entry) = entry else {
            return;
        };
        if let Ok(stored) = files.read_stored(entry) {
            spans.insert(entry.offset, stored);
        }
    })
}

/// The full on-demand flow, identical in both configurations: the bootstrap
/// read set is observed on file mounts, the same boot runs again on span
/// mounts with the blocking fetch handler, the second district collection is
/// requested, and the extended report returns together with the miss count
/// its on-demand load must have produced.
pub fn run_ondemand_probe() -> (ResidentBootReport, usize) {
    let root = game_root();
    observe_reads(record);
    let _ = run_probe(game_mounts());
    let bootstrap = recorded_reads();
    let (mounts, span_sets) = ondemand_span_mounts(&root, &bootstrap);

    let mut probe = ResidentBootProbe::new(mounts).expect("the boot probe starts");
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        match probe.poll().expect("the boot probe polls") {
            ResidentBootPoll::Done(_) => break,
            _ => {
                assert!(Instant::now() < deadline, "span boot probe timed out");
                std::thread::yield_now();
            }
        }
    }
    probe.request_more().expect("the second district loads");
    let report = loop {
        match probe.poll().expect("the boot probe polls") {
            ResidentBootPoll::Done(report) => break report,
            ResidentBootPoll::SecondResident => {
                assert!(Instant::now() < deadline, "on-demand load timed out");
                std::thread::yield_now();
            }
            _ => {
                assert!(Instant::now() < deadline, "on-demand load timed out");
                std::thread::yield_now();
            }
        }
    };
    eprintln!(
        "on-demand probe: done in {} stream polls, {} resident polls: {report:?}",
        probe.polls().0,
        probe.polls().1
    );
    let misses: usize = span_sets.iter().map(|spans| spans.misses().len()).sum();
    (report, misses)
}

/// The recorded native threaded report: the bootstrap (284 high collections,
/// finalized BAM bounds, the grind corpus collection's 21 plans, (21, 80)
/// retained) plus the sibling district's on-demand load through the blocking
/// miss handler — 7 more plans, the shared services' cumulative counts
/// (28, 124) at the second completion. Recorded once from the run above;
/// the threadless test asserts the identical value.
pub const EXPECTED: ResidentBootReport = ResidentBootReport {
    high_collections: 284,
    bounds_min: [3311468544, 3275851331, 3307372544],
    bounds_max: [1159069696, 1139775930, 1157234688],
    resident_plans: 21,
    retained_counts: (21, 80),
    second_plans: 7,
    second_retained: (28, 124),
};
