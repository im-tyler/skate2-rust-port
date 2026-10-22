//! The host worker policy is process-global, so its test runs alone in this
//! integration test's own process (`docs/WEB_PORT_M5_PLAN.md`, phase 2).

use std::io;

#[test]
fn spawn_worker_refuses_after_the_host_disables_workers() {
    skate2_core::host_thread::disable_worker_threads();
    let outcome = skate2_core::host_thread::spawn_worker(
        std::thread::Builder::new().name("policy probe".to_owned()),
        || (),
    );
    assert!(
        matches!(
            &outcome,
            Err(error) if error.kind() == io::ErrorKind::Unsupported
        ),
        "a disabled host refuses worker spawns: {outcome:?}"
    );
}
