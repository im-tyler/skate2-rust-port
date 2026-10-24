//! The host spawner (`docs/WEB_PORT_M7_PLAN.md`, P1) is process-global, so its
//! test runs alone in this integration test's own process. The fake host
//! starts threads itself (as the browser starts Web Workers) and records each
//! name; game threads must then behave as `std` threads do.

use std::sync::{Mutex, atomic::AtomicUsize, atomic::Ordering};

static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STARTED: AtomicUsize = AtomicUsize::new(0);

#[expect(
    clippy::unnecessary_wraps,
    reason = "a host spawner's signature: a real host's start can fail"
)]
fn fake_host(name: String, work: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    NAMES.lock().unwrap().push(name);
    STARTED.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(work);
    Ok(())
}

#[test]
fn game_threads_start_through_the_host_and_join_like_std_threads() {
    skate2_core::host_thread::install_spawner(fake_host).expect("first install");
    let (sender, receiver) = std::sync::mpsc::channel::<()>();
    let worker = skate2_core::host_thread::spawn("sk8 test worker", move || {
        receiver.recv().expect("release");
        7_u32
    })
    .expect("started");
    assert!(!worker.is_finished(), "still waiting for its release");
    sender.send(()).expect("release");
    assert_eq!(worker.join().expect("no panic"), 7);

    let optional =
        skate2_core::host_thread::spawn_worker("sk8 optional", || "done").expect("workers are on");
    assert_eq!(optional.join().expect("no panic"), "done");

    let panicking = skate2_core::host_thread::spawn("sk8 panics", || -> u32 {
        panic!("a game thread's panic reaches join")
    })
    .expect("started");
    assert!(
        panicking.join().is_err(),
        "a panic is join's error, as for std"
    );

    assert_eq!(STARTED.load(Ordering::SeqCst), 3);
    assert_eq!(
        *NAMES.lock().unwrap(),
        ["sk8 test worker", "sk8 optional", "sk8 panics"]
    );
    assert!(
        skate2_core::host_thread::install_spawner(fake_host).is_err(),
        "the first host stays installed"
    );
}
