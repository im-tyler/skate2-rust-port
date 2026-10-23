//! Shared harness for the resident boot probe corpus tests
//! (`docs/WEB_PORT_M5_PLAN.md`, phase 3). The two test binaries differ only
//! in `disable_worker_threads()`; everything else — including the expected
//! report — is byte-identical here.

#![allow(dead_code)]

use skate2_core::resident_boot_probe::{ResidentBootPoll, ResidentBootProbe, ResidentBootReport};
use skate2_formats::mount::{MountSet, MountedBig, SKATE2_ARCHIVES};
use std::{path::PathBuf, sync::Arc, time::Duration, time::Instant};

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

/// The recorded native threaded report (23,606 stream polls, 13,615
/// resident polls): 284 high collections, finalized BAM bounds, the grind
/// corpus collection's 21 plans. Recorded once from the run above; the
/// threadless test asserts the identical value.
pub const EXPECTED: ResidentBootReport = ResidentBootReport {
    high_collections: 284,
    bounds_min: [3311468544, 3275851331, 3307372544],
    bounds_max: [1159069696, 1139775930, 1157234688],
    resident_plans: 21,
    retained_counts: (21, 80),
};
