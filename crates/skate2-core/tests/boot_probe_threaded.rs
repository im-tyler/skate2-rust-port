//! The resident boot probe on native threads — the oracle recording run
//! (`docs/WEB_PORT_M5_PLAN.md`, phase 3). The identical threadless run in
//! `boot_probe_threadless` proves the browser's configuration.

mod common;

#[test]
#[ignore = "requires external SKATE2_GAME_ROOT"]
fn threaded_boot_probe_matches_the_recorded_report() {
    let report = common::run_probe(common::game_mounts());
    assert_eq!(report, common::EXPECTED, "the threaded boot is the oracle");
}
