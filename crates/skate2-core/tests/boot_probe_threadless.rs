//! The resident boot probe with worker threads disabled — the exact
//! configuration the browser runs (`docs/WEB_PORT_M5_PLAN.md`, phase 3;
//! milestone 5a's on-demand extension, `docs/WEB_PORT_M5A_PLAN.md`,
//! phase 4). Own process because the host policy is global; it must produce
//! the identical report the threaded oracle recorded, including the second
//! district's on-demand load.

mod common;

#[test]
#[ignore = "requires external SKATE2_GAME_ROOT"]
fn threadless_boot_probe_matches_the_threaded_report() {
    skate2_core::host_thread::disable_worker_threads();
    let (report, misses) = common::run_ondemand_probe();
    assert!(
        misses > 0,
        "the second district loaded on demand, through misses"
    );
    assert_eq!(
        report,
        common::EXPECTED,
        "threadless and threaded boots are behaviorally identical"
    );
}
