//! The resident boot probe with worker threads disabled — the exact
//! configuration the browser runs (`docs/WEB_PORT_M5_PLAN.md`, phase 3).
//! Own process because the host policy is global; it must produce the
//! identical report the threaded oracle recorded.

mod common;

#[test]
#[ignore = "requires external SKATE2_GAME_ROOT"]
fn threadless_boot_probe_matches_the_threaded_report() {
    skate2_core::host_thread::disable_worker_threads();
    let report = common::run_probe(common::game_mounts());
    assert_eq!(
        report,
        common::EXPECTED,
        "threadless and threaded boots are behaviorally identical"
    );
}
