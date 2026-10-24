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
        Arc, Condvar, Mutex, OnceLock,
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

/// A host's thread starter, for platforms where `std::thread` cannot spawn
/// (the threaded browser build starts threads as Web Workers): runs `work` on
/// a new thread named `name` and returns once it started.
pub type HostSpawner = fn(name: String, work: Box<dyn FnOnce() + Send>) -> io::Result<()>;

static SPAWNER: OnceLock<HostSpawner> = OnceLock::new();

/// Installs the host's thread starter, once, before any game work; every
/// game thread ([`spawn`], [`spawn_worker`]) then starts through it.
///
/// # Errors
/// A starter was already installed; it stays.
pub fn install_spawner(spawner: HostSpawner) -> Result<(), HostSpawner> {
    SPAWNER.set(spawner)
}

/// A started game thread: a `std` thread, or one the host's starter started.
#[derive(Debug)]
pub struct WorkerHandle<T>(Handle<T>);

#[derive(Debug)]
enum Handle<T> {
    Std(JoinHandle<T>),
    Host(Arc<HostResult<T>>),
}

/// A host-started thread's outcome, set once when its work returns.
#[derive(Debug)]
struct HostResult<T> {
    value: Mutex<Option<thread::Result<T>>>,
    done: Condvar,
}

impl<T> WorkerHandle<T> {
    /// Whether the thread's work has returned, as `JoinHandle::is_finished`.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        match &self.0 {
            Handle::Std(handle) => handle.is_finished(),
            Handle::Host(result) => result
                .value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
        }
    }

    /// Waits for the thread, as `JoinHandle::join`.
    ///
    /// # Errors
    /// The work panicked.
    pub fn join(self) -> thread::Result<T> {
        match self.0 {
            Handle::Std(handle) => handle.join(),
            Handle::Host(result) => {
                let mut value = result
                    .value
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                loop {
                    if let Some(outcome) = value.take() {
                        return outcome;
                    }
                    value = result
                        .done
                        .wait(value)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

/// Starts a game thread named `name`: through the host's starter when one is
/// installed, else as a named `std` thread (exactly as before the seam).
///
/// # Errors
/// The thread could not start.
pub fn spawn<F, T>(name: impl Into<String>, work: F) -> io::Result<WorkerHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let name = name.into();
    let Some(spawner) = SPAWNER.get() else {
        return thread::Builder::new()
            .name(name)
            .spawn(work)
            .map(|handle| WorkerHandle(Handle::Std(handle)));
    };
    let result = Arc::new(HostResult {
        value: Mutex::new(None),
        done: Condvar::new(),
    });
    let slot = Arc::clone(&result);
    spawner(
        name,
        Box::new(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            *slot
                .value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
            slot.done.notify_all();
        }),
    )?;
    Ok(WorkerHandle(Handle::Host(result)))
}

/// [`spawn`] for an optional worker, or `Unsupported` when the host disabled
/// workers — so callers' existing spawn-failure fallbacks decide how the work
/// runs inline. Only for optional workers whose absence has a documented
/// degradation; pools without one use [`spawn`].
///
/// # Errors
///
/// The thread could not start, or `Unsupported` when the host disabled
/// worker threads.
pub fn spawn_worker<F, T>(name: impl Into<String>, work: F) -> io::Result<WorkerHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    if worker_threads_available() {
        spawn(name, work)
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
///
/// # Errors
/// Hooks were already installed; they stay.
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
