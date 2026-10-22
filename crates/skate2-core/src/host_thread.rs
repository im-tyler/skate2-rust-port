//! Optional host notifications for workers on the frame's dependency path.
//!
//! These lifetime hooks let a platform describe its threads to the OS. They do
//! not choose jobs, change affinity masks, or participate in simulation timing.
//!
//! The worker policy below lets a host opt out of worker threads entirely (the
//! browser build, which has none): the optional workers route their spawns
//! through [`spawn_worker`] and then do their work on the calling thread, and
//! the read scheduler runs its admitted reads during `poll` instead.
use std::{
    io,
    marker::PhantomData,
    rc::Rc,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

static WORKER_THREADS_DISABLED: AtomicBool = AtomicBool::new(false);

/// The host cannot run worker threads (the browser build). Optional workers
/// then do their work on the calling thread. Call once, before any game work:
/// workers already started, and lazily started readers, are not affected.
pub fn disable_worker_threads() {
    WORKER_THREADS_DISABLED.store(true, Ordering::Release);
}

/// Whether the host still allows worker threads.
pub fn worker_threads_available() -> bool {
    !WORKER_THREADS_DISABLED.load(Ordering::Acquire)
}

/// `builder.spawn(work)`, or `Unsupported` when the host disabled workers —
/// so callers' existing spawn-failure fallbacks decide how the work runs
/// inline. Only for optional workers whose absence has a documented
/// degradation; pools without one must not route through this.
///
/// # Errors
///
/// Returns the builder's own spawn error, or `Unsupported` when the host
/// disabled worker threads.
pub fn spawn_worker<F, T>(builder: thread::Builder, work: F) -> io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    if worker_threads_available() {
        builder.spawn(work)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "worker threads disabled by the host",
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkerThreadHooks {
    /// Called on the worker before any work. Must not panic.
    pub entered: fn(),
    /// Called on the same worker before it exits. Must not panic.
    pub leaving: fn(),
}

static HOOKS: OnceLock<WorkerThreadHooks> = OnceLock::new();

/// Install once, before starting any game workers. With no hooks installed,
/// worker lifetime registration has no platform effect.
pub fn install_worker_thread_hooks(hooks: WorkerThreadHooks) -> Result<(), WorkerThreadHooks> {
    HOOKS.set(hooks)
}

/// A worker's registration cannot move to another thread: its exit notification
/// must identify the same OS thread, including during unwinding.
#[must_use]
pub struct WorkerThreadScope {
    leaving: Option<fn()>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl WorkerThreadScope {
    pub fn enter() -> Self {
        let hooks = HOOKS.get();
        if let Some(hooks) = hooks {
            (hooks.entered)();
        }
        Self {
            leaving: hooks.map(|hooks| hooks.leaving),
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for WorkerThreadScope {
    fn drop(&mut self) {
        if let Some(leaving) = self.leaving {
            leaving();
        }
    }
}
