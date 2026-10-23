//! Mount-backed asynchronous reads for the reconstructed inner world stream.
//!
//! The reference separates the `AppMan` load thread from filesystem device
//! threads. `xcore::FileLoader::ASyncLoad` (`0x82973630`) owns 128 operations,
//! and `StartNextLoad` (`0x82972f30`) admits at most three at once. Device
//! callbacks only make an operation terminal; `ASyncLoadOp::Release`
//! (`0x82975510`) is what frees its active lease and admits another request.
//!
//! This module keeps the same observable boundary. Host workers only call
//! [`MountSet::read_path`]. Parsing and every registration callback remain on
//! the thread that polls [`InnerStreamBackend`]. Worker implementation and
//! shutdown are safe PC policy; native device seek reordering and cancellation
//! are intentionally not reconstructed here. Dropping the backend waits for
//! every active host read to return and joins all workers synchronously. There
//! is no cancellation path and therefore no finite destructor-time guarantee
//! if a host filesystem read never returns.

use crate::game_stream::{
    AsyncReadPoll, AsyncReadRequest, InnerStreamBackend, ReadStage, StreamRegistrationContext,
};
use skate2_formats::{
    mount::{MountError, MountSet},
    stream::{AssetToc, CollectionMap, CollectionSpace},
};
use std::{
    collections::{HashMap, VecDeque},
    fmt, io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
};

/// Exact number of operations owned by the reference `xcore::FileLoader`.
pub const ASYNC_OPERATION_CAPACITY: usize = 128;

/// Exact maximum active count enforced by `FileLoader::StartNextLoad`.
pub const MAX_ACTIVE_ASYNC_READS: usize = 3;

const NATIVE_PATH_CAPACITY: usize = 256;
const NATIVE_READ_ALIGNMENT: u32 = 0x10;

/// Registration effects performed after bytes are returned to the load thread.
///
/// This is mandatory: a production backend cannot silently discard collection,
/// asset, spatial, XML, or unload ownership changes. Implementations are called
/// synchronously by [`InnerStreamBackend`] methods and are never moved into the
/// host read workers.
pub trait StreamRegistrationSink {
    type Error;

    /// Applies the parsed XSM collection registrations.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the registrations cannot be applied.
    fn collection_map_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        map: &CollectionMap,
    ) -> Result<(), Self::Error>;

    /// Applies the parsed XST asset registrations.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the registrations cannot be applied.
    fn asset_toc_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        toc: &AssetToc,
    ) -> Result<(), Self::Error>;

    /// Applies the parsed XSS ownership after adapter processing.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the spatial ownership cannot be applied.
    fn collection_space_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        space: &CollectionSpace,
    ) -> Result<(), Self::Error>;

    /// Applies opaque stream XML bytes.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the XML ownership cannot be applied.
    fn stream_xml_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;

    /// Removes XSM collection registrations.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the registrations cannot be removed.
    fn collection_map_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        map: &CollectionMap,
    ) -> Result<(), Self::Error>;

    /// Removes XST asset registrations.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the registrations cannot be removed.
    fn asset_toc_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        toc: &AssetToc,
    ) -> Result<(), Self::Error>;

    /// Releases the file-level XSS/XML ownership.
    ///
    /// # Errors
    ///
    /// Returns the collaborator error if the ownership cannot be released.
    fn stream_file_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        space: Option<&CollectionSpace>,
        had_stream_xml: bool,
    ) -> Result<(), Self::Error>;
}

/// Invalid request rejected before it can enter the worker queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountedReadRequestError {
    InteriorNul,
    PathTooLong {
        byte_len: usize,
        capacity: usize,
    },
    UnsupportedArchiveNamespace,
    UnexpectedFlags {
        stage: ReadStage,
        expected: u32,
        actual: u32,
    },
    UnexpectedAlignment {
        expected: u32,
        actual: u32,
    },
}

impl fmt::Display for MountedReadRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InteriorNul => formatter.write_str("async read path contains an interior NUL"),
            Self::PathTooLong { byte_len, capacity } => write!(
                formatter,
                "async read path is {byte_len} bytes; native capacity is {capacity} including NUL"
            ),
            Self::UnsupportedArchiveNamespace => formatter.write_str(
                "MountSet does not implement the reference big-file `name|path` namespace",
            ),
            Self::UnexpectedFlags {
                stage,
                expected,
                actual,
            } => write!(
                formatter,
                "{stage:?} read has flags {actual:#x}; expected {expected:#x}"
            ),
            Self::UnexpectedAlignment { expected, actual } => write!(
                formatter,
                "async read has alignment {actual:#x}; expected {expected:#x}"
            ),
        }
    }
}

impl std::error::Error for MountedReadRequestError {}

/// Failure of the safe host operation scheduler rather than a file read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountedReadSchedulerError {
    OperationPoolExhausted,
    Poisoned,
    WorkerPanicked,
    /// Defensive internal state used while workers are being joined.
    ///
    /// Ordinary safe public use cannot poll or begin through an already
    /// consumed backend, so this is not normally observable to callers.
    ShuttingDown,
    ForeignHandle,
    MissingOperation,
    LeaseAccountingCorrupt,
    CompleteResultAlreadyTaken,
}

impl fmt::Display for MountedReadSchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationPoolExhausted => {
                formatter.write_str("async operation pool is exhausted")
            }
            Self::Poisoned => formatter.write_str("async read scheduler mutex is poisoned"),
            Self::WorkerPanicked => formatter.write_str("async read worker panicked"),
            Self::ShuttingDown => formatter.write_str("async read scheduler is shutting down"),
            Self::ForeignHandle => {
                formatter.write_str("async read handle belongs to another backend")
            }
            Self::MissingOperation => {
                formatter.write_str("async read operation is no longer owned")
            }
            Self::LeaseAccountingCorrupt => {
                formatter.write_str("async read active-lease accounting is corrupt")
            }
            Self::CompleteResultAlreadyTaken => {
                formatter.write_str("completed async read bytes were already transferred")
            }
        }
    }
}

impl std::error::Error for MountedReadSchedulerError {}

/// Error surfaced by [`MountedStreamBackend`].
#[derive(Debug)]
pub enum MountedStreamBackendError<SinkError> {
    Request(MountedReadRequestError),
    Scheduler(MountedReadSchedulerError),
    Registration(SinkError),
}

impl<SinkError: fmt::Display> fmt::Display for MountedStreamBackendError<SinkError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
            Self::Scheduler(error) => error.fmt(formatter),
            Self::Registration(error) => write!(formatter, "stream registration failed: {error}"),
        }
    }
}

impl<SinkError> std::error::Error for MountedStreamBackendError<SinkError> where
    SinkError: std::error::Error + 'static
{
}

trait ReadSource: Send + Sync + 'static {
    fn read(&self, request: &AsyncReadRequest) -> SourceRead;
}

enum SourceRead {
    Complete(Vec<u8>),
    Failed,
    /// The source is not ready for this request — resident-span storage
    /// whose bytes have not been fetched yet. The scheduler re-attempts the
    /// operation on a later poll; a file-backed source never produces this
    /// (files do not answer `WouldBlock`), so native behavior is unchanged.
    Retry,
}

struct MountReadSource {
    mounts: Arc<MountSet>,
}

impl ReadSource for MountReadSource {
    fn read(&self, request: &AsyncReadRequest) -> SourceRead {
        match self.mounts.read_path(&request.path) {
            Ok(bytes) => SourceRead::Complete(bytes),
            // A span miss: not an error, a not-yet. The host's miss handler
            // has already kicked the fetch; the next poll re-reads.
            Err(MountError::Io { source, .. }) if source.kind() == io::ErrorKind::WouldBlock => {
                SourceRead::Retry
            }
            Err(_) => SourceRead::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationPhase {
    Pending,
    Admitted,
    Reading,
    Complete,
    Failed,
}

struct Operation {
    request: AsyncReadRequest,
    phase: OperationPhase,
    bytes: Option<Vec<u8>>,
    handle_alive: bool,
}

struct SchedulerState {
    next_id: u64,
    operations: HashMap<u64, Operation>,
    pending: VecDeque<u64>,
    ready: VecDeque<u64>,
    active_leases: usize,
    shutting_down: bool,
    fault: Option<MountedReadSchedulerError>,
    #[cfg(test)]
    admission_order: Vec<u64>,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            next_id: 1,
            operations: HashMap::with_capacity(ASYNC_OPERATION_CAPACITY),
            pending: VecDeque::with_capacity(ASYNC_OPERATION_CAPACITY),
            ready: VecDeque::with_capacity(MAX_ACTIVE_ASYNC_READS),
            active_leases: 0,
            shutting_down: false,
            fault: None,
            #[cfg(test)]
            admission_order: Vec::new(),
        }
    }

    fn admit_pending(&mut self) -> bool {
        let mut admitted = false;
        while self.active_leases < MAX_ACTIVE_ASYNC_READS {
            let Some(id) = self.pending.pop_front() else {
                break;
            };
            let Some(operation) = self.operations.get_mut(&id) else {
                continue;
            };
            if operation.phase != OperationPhase::Pending || !operation.handle_alive {
                continue;
            }
            operation.phase = OperationPhase::Admitted;
            self.ready.push_back(id);
            self.active_leases += 1;
            #[cfg(test)]
            self.admission_order.push(id);
            admitted = true;
        }
        admitted
    }

    fn next_operation_id(&mut self) -> u64 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1);
            if !self.operations.contains_key(&id) {
                return id;
            }
        }
    }

    fn release_active_lease(&mut self) -> bool {
        if self.active_leases == 0 {
            self.fault = Some(MountedReadSchedulerError::LeaseAccountingCorrupt);
            false
        } else {
            self.active_leases -= 1;
            true
        }
    }
}

struct SchedulerCore {
    source: Arc<dyn ReadSource>,
    state: Mutex<SchedulerState>,
    wake: Condvar,
}

impl SchedulerCore {
    fn checked_state(&self) -> Result<MutexGuard<'_, SchedulerState>, MountedReadSchedulerError> {
        match self.state.lock() {
            Ok(state) => {
                if state.shutting_down {
                    Err(MountedReadSchedulerError::ShuttingDown)
                } else if let Some(error) = state.fault {
                    Err(error)
                } else {
                    Ok(state)
                }
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.fault = Some(MountedReadSchedulerError::Poisoned);
                self.wake.notify_all();
                Err(MountedReadSchedulerError::Poisoned)
            }
        }
    }

    fn release_handle(&self, id: u64) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.fault = Some(MountedReadSchedulerError::Poisoned);
                self.wake.notify_all();
                return;
            }
        };
        let Some(operation) = state.operations.get_mut(&id) else {
            return;
        };
        operation.handle_alive = false;
        match operation.phase {
            OperationPhase::Pending => {
                // Safe PC abandonment policy: an unadmitted operation has not
                // touched the source, so dropping its sole handle discards it.
                // This is not a claim about native `Cancel` behavior.
                state.operations.remove(&id);
                state.pending.retain(|pending_id| *pending_id != id);
            }
            OperationPhase::Complete | OperationPhase::Failed => {
                state.operations.remove(&id);
                let lease_released = state.release_active_lease();
                if lease_released && state.fault.is_none() && !state.shutting_down {
                    state.admit_pending();
                }
            }
            OperationPhase::Admitted | OperationPhase::Reading => {
                // Safe PC policy: no native cancellation is inferred. The
                // worker keeps the lease until it reaches a terminal result.
            }
        }
        self.wake.notify_all();
    }

    fn mark_fault(&self, fault: MountedReadSchedulerError) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.fault.get_or_insert(fault);
        self.wake.notify_all();
    }
}

/// Owned operation handle returned to the inner stream state machine.
///
/// Dropping a terminal handle corresponds to the supported reference
/// `ASyncLoadOp::Release` ownership point. Dropping an in-flight handle marks it
/// abandoned but does not free its active lease until the host read terminates.
pub struct MountedReadHandle {
    id: u64,
    core: Arc<SchedulerCore>,
}

impl fmt::Debug for MountedReadHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountedReadHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for MountedReadHandle {
    fn drop(&mut self) {
        self.core.release_handle(self.id);
    }
}

struct ReadScheduler {
    core: Arc<SchedulerCore>,
    workers: Vec<JoinHandle<()>>,
    /// Inline mode (host policy: no worker threads): admitted reads run on
    /// the polling thread during `poll`, through the same state machine.
    inline: bool,
}

impl ReadScheduler {
    fn new(source: Arc<dyn ReadSource>) -> Self {
        if !crate::host_thread::worker_threads_available() {
            return Self::inline(source);
        }
        let core = Arc::new(SchedulerCore {
            source,
            state: Mutex::new(SchedulerState::new()),
            wake: Condvar::new(),
        });
        let mut workers = Vec::with_capacity(MAX_ACTIVE_ASYNC_READS);
        for index in 0..MAX_ACTIVE_ASYNC_READS {
            let worker_core = Arc::clone(&core);
            match crate::host_thread::spawn_worker(
                thread::Builder::new().name(format!("skate2-stream-read-{index}")),
                move || {
                    if catch_unwind(AssertUnwindSafe(|| worker_main(&worker_core))).is_err() {
                        worker_core.mark_fault(MountedReadSchedulerError::WorkerPanicked);
                    }
                },
            ) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    // A worker could not start. The pool is all-or-nothing:
                    // shut the started ones down and join them, then serve
                    // every read inline — the same degradation the host
                    // policy produces, so a thread-starved native process
                    // keeps loading rather than failing.
                    {
                        let mut state = match core.state.lock() {
                            Ok(state) => state,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        state.shutting_down = true;
                        core.wake.notify_all();
                    }
                    for worker in workers {
                        let _ = worker.join();
                    }
                    let _ = error;
                    // A fresh core: the abandoned one's operations died with
                    // its workers, and no request has been served yet.
                    return Self::inline(Arc::clone(&core.source));
                }
            }
        }
        Self {
            core,
            workers,
            inline: false,
        }
    }

    /// A scheduler with no workers: reads run during `poll`. The pool, the
    /// admission limit, and the FIFO order are the workers' own.
    fn inline(source: Arc<dyn ReadSource>) -> Self {
        Self {
            core: Arc::new(SchedulerCore {
                source,
                state: Mutex::new(SchedulerState::new()),
                wake: Condvar::new(),
            }),
            workers: Vec::new(),
            inline: true,
        }
    }

    /// Runs every admitted read on the calling thread until none is ready or
    /// a fault is set: the worker's loop without the waiting. An operation
    /// that answers [`SourceRead::Retry`] returns to the ready front for
    /// the next poll — one attempt per pass — so a not-ready source cannot
    /// turn this loop into a spin.
    fn run_ready_inline(&self) {
        let mut retried: Vec<u64> = Vec::new();
        loop {
            let next = {
                loop {
                    let mut state = match self.core.state.lock() {
                        Ok(state) => state,
                        Err(poisoned) => {
                            let mut state = poisoned.into_inner();
                            state.fault = Some(MountedReadSchedulerError::Poisoned);
                            self.core.wake.notify_all();
                            break None;
                        }
                    };
                    if state.shutting_down || state.fault.is_some() {
                        break None;
                    }
                    let Some(id) = state.ready.pop_front() else {
                        break None;
                    };
                    if retried.contains(&id) {
                        continue;
                    }
                    let Some(operation) = state.operations.get_mut(&id) else {
                        continue;
                    };
                    operation.phase = OperationPhase::Reading;
                    break Some((id, operation.request.clone()));
                }
            };
            let Some((id, request)) = next else {
                break;
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| self.core.source.read(&request)));
            if matches!(outcome, Ok(SourceRead::Retry)) {
                retried.push(id);
            }
            record_outcome(&self.core, id, outcome);
        }
        if !retried.is_empty() {
            let mut state = match self.core.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            // Exactly once each, at the front, in the order attempted:
            // record_outcome already re-queued every retried id, so a copy
            // still queued when the pass ended is dropped here first.
            state.ready.retain(|queued| !retried.contains(queued));
            for id in retried.iter().rev() {
                state.ready.push_front(*id);
            }
            self.core.wake.notify_all();
        }
    }

    fn begin(
        &self,
        request: AsyncReadRequest,
    ) -> Result<MountedReadHandle, MountedReadSchedulerError> {
        let mut state = self.core.checked_state()?;
        if state.operations.len() >= ASYNC_OPERATION_CAPACITY {
            return Err(MountedReadSchedulerError::OperationPoolExhausted);
        }
        let id = state.next_operation_id();
        state.operations.insert(
            id,
            Operation {
                request,
                phase: OperationPhase::Pending,
                bytes: None,
                handle_alive: true,
            },
        );
        state.pending.push_back(id);
        if state.admit_pending() {
            self.core.wake.notify_all();
        }
        Ok(MountedReadHandle {
            id,
            core: Arc::clone(&self.core),
        })
    }

    fn poll(
        &self,
        handle: &mut MountedReadHandle,
    ) -> Result<AsyncReadPoll, MountedReadSchedulerError> {
        // Inline mode runs the admitted reads here: this poll call is the
        // "worker". Worker mode's workers have usually finished already.
        if self.inline {
            self.run_ready_inline();
        }
        if !Arc::ptr_eq(&self.core, &handle.core) {
            return Err(MountedReadSchedulerError::ForeignHandle);
        }
        let mut state = self.core.checked_state()?;
        let operation = state
            .operations
            .get_mut(&handle.id)
            .ok_or(MountedReadSchedulerError::MissingOperation)?;
        match operation.phase {
            OperationPhase::Pending | OperationPhase::Admitted | OperationPhase::Reading => {
                Ok(AsyncReadPoll::Pending)
            }
            OperationPhase::Complete => operation
                .bytes
                .take()
                .map(AsyncReadPoll::Complete)
                .ok_or(MountedReadSchedulerError::CompleteResultAlreadyTaken),
            OperationPhase::Failed => Ok(AsyncReadPoll::Failed),
        }
    }
}

impl Drop for ReadScheduler {
    fn drop(&mut self) {
        {
            let mut state = match self.core.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.shutting_down = true;
            self.core.wake.notify_all();
        }
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                self.core
                    .mark_fault(MountedReadSchedulerError::WorkerPanicked);
            }
        }
    }
}

fn worker_main(core: &SchedulerCore) {
    loop {
        let (id, request) = {
            let mut state = match core.state.lock() {
                Ok(state) => state,
                Err(poisoned) => {
                    let mut state = poisoned.into_inner();
                    state.fault = Some(MountedReadSchedulerError::Poisoned);
                    core.wake.notify_all();
                    return;
                }
            };
            loop {
                if state.shutting_down || state.fault.is_some() {
                    return;
                }
                if let Some(id) = state.ready.pop_front() {
                    let Some(operation) = state.operations.get_mut(&id) else {
                        continue;
                    };
                    operation.phase = OperationPhase::Reading;
                    break (id, operation.request.clone());
                }
                state = match core.wake.wait(state) {
                    Ok(state) => state,
                    Err(poisoned) => {
                        let mut state = poisoned.into_inner();
                        state.fault = Some(MountedReadSchedulerError::Poisoned);
                        core.wake.notify_all();
                        return;
                    }
                };
            }
        };

        let outcome = catch_unwind(AssertUnwindSafe(|| core.source.read(&request)));
        record_outcome(core, id, outcome);
    }
}

/// Completes one admitted read and records its outcome through the shared
/// state, exactly as the worker does after its `source.read` returns — and
/// as inline mode does during `poll`. A missing operation or a poisoned
/// lock latches the same fault either way.
fn record_outcome(core: &SchedulerCore, id: u64, outcome: thread::Result<SourceRead>) {
    let mut state = match core.state.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.fault = Some(MountedReadSchedulerError::Poisoned);
            core.wake.notify_all();
            return;
        }
    };
    let (handle_alive, worker_panicked, retry) = {
        let Some(operation) = state.operations.get_mut(&id) else {
            state.fault = Some(MountedReadSchedulerError::MissingOperation);
            core.wake.notify_all();
            return;
        };
        let (worker_panicked, retry) = match outcome {
            Ok(SourceRead::Complete(bytes)) => {
                operation.phase = OperationPhase::Complete;
                operation.bytes = Some(bytes);
                (false, false)
            }
            Ok(SourceRead::Failed) => {
                operation.phase = OperationPhase::Failed;
                (false, false)
            }
            // Not ready: the operation returns to the ready front as if
            // never attempted — FIFO preserved, the admission lease held,
            // the next poll re-reads (by then the host's fetch has usually
            // made the span resident). The yield guards a worker-mode
            // against spinning; unreachable in production, where only the
            // browser's inline mode sees Retry.
            Ok(SourceRead::Retry) => {
                operation.phase = OperationPhase::Admitted;
                (false, true)
            }
            Err(_) => {
                operation.phase = OperationPhase::Failed;
                (true, false)
            }
        };
        (operation.handle_alive, worker_panicked, retry)
    };
    if worker_panicked {
        state.fault = Some(MountedReadSchedulerError::WorkerPanicked);
    }
    if retry && handle_alive {
        state.ready.push_front(id);
    }
    if !handle_alive {
        state.operations.remove(&id);
        let lease_released = state.release_active_lease();
        if lease_released && state.fault.is_none() && !state.shutting_down {
            state.admit_pending();
        }
    }
    core.wake.notify_all();
    if retry {
        thread::yield_now();
    }
}

/// Concrete mount-backed implementation of [`InnerStreamBackend`].
///
/// Destruction synchronously joins the host workers. Because this safe PC
/// boundary does not invent native cancellation, dropping a backend can wait
/// indefinitely when an active [`MountSet::read_path`] never returns.
pub struct MountedStreamBackend<Sink> {
    scheduler: ReadScheduler,
    sink: Sink,
}

impl<Sink> MountedStreamBackend<Sink>
where
    Sink: StreamRegistrationSink,
{
    /// Creates the three host workers used by the safe PC scheduler.
    ///
    /// # Errors
    ///
    /// Returns [`MountedStreamBackendError::WorkerSpawn`] if a worker cannot be
    /// started. Already-started workers are stopped and joined before return.
    pub fn new(
        mounts: Arc<MountSet>,
        sink: Sink,
    ) -> Result<Self, MountedStreamBackendError<Sink::Error>> {
        let source = Arc::new(MountReadSource { mounts });
        Self::with_source(source, sink)
    }

    fn with_source(
        source: Arc<dyn ReadSource>,
        sink: Sink,
    ) -> Result<Self, MountedStreamBackendError<Sink::Error>> {
        // A worker that cannot start degrades to inline reads, so the
        // scheduler always constructs: host policy, not an error.
        let scheduler = ReadScheduler::new(source);
        Ok(Self { scheduler, sink })
    }

    #[must_use]
    pub const fn registration_sink(&self) -> &Sink {
        &self.sink
    }

    /// Returns the sink after synchronously shutting down and joining workers.
    ///
    /// Like dropping the backend, this has no finite completion guarantee if
    /// an active host read never returns; no cancellation behavior is inferred.
    #[must_use]
    pub fn into_registration_sink(self) -> Sink {
        self.sink
    }
}

fn validate_request(request: &AsyncReadRequest) -> Result<(), MountedReadRequestError> {
    if request.path.as_bytes().contains(&0) {
        return Err(MountedReadRequestError::InteriorNul);
    }
    if request.path.len() >= NATIVE_PATH_CAPACITY {
        return Err(MountedReadRequestError::PathTooLong {
            byte_len: request.path.len(),
            capacity: NATIVE_PATH_CAPACITY,
        });
    }
    if request.path.as_bytes().contains(&b'|') {
        return Err(MountedReadRequestError::UnsupportedArchiveNamespace);
    }
    let expected_flags = match request.stage {
        ReadStage::CollectionMap | ReadStage::AssetToc => 1,
        ReadStage::CollectionSpace | ReadStage::StreamXml => 0,
    };
    if request.flags != expected_flags {
        return Err(MountedReadRequestError::UnexpectedFlags {
            stage: request.stage,
            expected: expected_flags,
            actual: request.flags,
        });
    }
    if request.alignment != NATIVE_READ_ALIGNMENT {
        return Err(MountedReadRequestError::UnexpectedAlignment {
            expected: NATIVE_READ_ALIGNMENT,
            actual: request.alignment,
        });
    }
    Ok(())
}

impl<Sink> InnerStreamBackend for MountedStreamBackend<Sink>
where
    Sink: StreamRegistrationSink,
{
    type Handle = MountedReadHandle;
    type Error = MountedStreamBackendError<Sink::Error>;

    fn begin_read(&mut self, request: AsyncReadRequest) -> Result<Self::Handle, Self::Error> {
        validate_request(&request).map_err(MountedStreamBackendError::Request)?;
        self.scheduler
            .begin(request)
            .map_err(MountedStreamBackendError::Scheduler)
    }

    fn poll_read(&mut self, handle: &mut Self::Handle) -> Result<AsyncReadPoll, Self::Error> {
        self.scheduler
            .poll(handle)
            .map_err(MountedStreamBackendError::Scheduler)
    }

    fn collection_map_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        map: &CollectionMap,
    ) -> Result<(), Self::Error> {
        self.sink
            .collection_map_loaded(context, map)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn asset_toc_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        toc: &AssetToc,
    ) -> Result<(), Self::Error> {
        self.sink
            .asset_toc_loaded(context, toc)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn collection_space_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        space: &CollectionSpace,
    ) -> Result<(), Self::Error> {
        self.sink
            .collection_space_loaded(context, space)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn stream_xml_loaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.sink
            .stream_xml_loaded(context, bytes)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn collection_map_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        map: &CollectionMap,
    ) -> Result<(), Self::Error> {
        self.sink
            .collection_map_unloaded(context, map)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn asset_toc_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        toc: &AssetToc,
    ) -> Result<(), Self::Error> {
        self.sink
            .asset_toc_unloaded(context, toc)
            .map_err(MountedStreamBackendError::Registration)
    }

    fn stream_file_unloaded(
        &mut self,
        context: StreamRegistrationContext<'_>,
        space: Option<&CollectionSpace>,
        had_stream_xml: bool,
    ) -> Result<(), Self::Error> {
        self.sink
            .stream_file_unloaded(context, space, had_stream_xml)
            .map_err(MountedStreamBackendError::Registration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_stream::{
        GameStreamState, StreamFile, StreamFileState, StreamKind, StreamXmlState,
    };
    use std::{
        collections::{HashMap, HashSet},
        convert::Infallible,
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[derive(Clone)]
    enum TestOutcome {
        Complete(Vec<u8>),
        Failed,
        Panic,
        /// The source is not ready: the read answers `Retry`.
        NotReady,
    }

    #[derive(Default)]
    struct TestSourceState {
        outcomes: HashMap<String, TestOutcome>,
        blocked: HashSet<String>,
        released: HashSet<String>,
        started: Vec<AsyncReadRequest>,
        active: usize,
        maximum_active: usize,
    }

    #[derive(Default)]
    struct TestSource {
        state: Mutex<TestSourceState>,
        changed: Condvar,
    }

    impl TestSource {
        fn set_outcome(&self, path: &str, outcome: TestOutcome) {
            self.state
                .lock()
                .unwrap()
                .outcomes
                .insert(path.to_owned(), outcome);
        }

        fn block(&self, path: &str) {
            self.state.lock().unwrap().blocked.insert(path.to_owned());
        }

        fn release(&self, path: &str) {
            self.state.lock().unwrap().released.insert(path.to_owned());
            self.changed.notify_all();
        }

        fn release_all(&self) {
            let mut state = self.state.lock().unwrap();
            let blocked: Vec<_> = state.blocked.iter().cloned().collect();
            state.released.extend(blocked);
            self.changed.notify_all();
        }

        fn wait_for_started(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = self.state.lock().unwrap();
            while state.started.len() < count {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("timed out waiting for worker start");
                let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!timeout.timed_out(), "timed out waiting for worker start");
            }
        }

        fn wait_for_path_started(&self, path: &str) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = self.state.lock().unwrap();
            while !state.started.iter().any(|request| request.path == path) {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("timed out waiting for worker path");
                let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
                assert!(!timeout.timed_out(), "timed out waiting for {path}");
            }
        }

        fn started_paths(&self) -> Vec<String> {
            self.state
                .lock()
                .unwrap()
                .started
                .iter()
                .map(|request| request.path.clone())
                .collect()
        }

        fn started_requests(&self) -> Vec<AsyncReadRequest> {
            self.state.lock().unwrap().started.clone()
        }

        fn maximum_active(&self) -> usize {
            self.state.lock().unwrap().maximum_active
        }
    }

    impl ReadSource for TestSource {
        fn read(&self, request: &AsyncReadRequest) -> SourceRead {
            let mut state = self.state.lock().unwrap();
            state.started.push(request.clone());
            state.active += 1;
            state.maximum_active = state.maximum_active.max(state.active);
            self.changed.notify_all();
            while state.blocked.contains(&request.path) && !state.released.contains(&request.path) {
                state = self.changed.wait(state).unwrap();
            }
            let outcome = state
                .outcomes
                .get(&request.path)
                .cloned()
                .unwrap_or_else(|| TestOutcome::Complete(request.path.as_bytes().to_vec()));
            state.active -= 1;
            self.changed.notify_all();
            drop(state);
            match outcome {
                TestOutcome::Complete(bytes) => SourceRead::Complete(bytes),
                TestOutcome::Failed => SourceRead::Failed,
                TestOutcome::Panic => panic!("injected worker-source panic"),
                TestOutcome::NotReady => SourceRead::Retry,
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum SinkEvent {
        MapLoaded(StreamKind, String, usize, thread::ThreadId),
        TocLoaded(StreamKind, usize, thread::ThreadId),
        SpaceLoaded(StreamKind, usize, thread::ThreadId),
        XmlLoaded(StreamKind, Vec<u8>, thread::ThreadId),
        MapUnloaded(StreamKind, usize, thread::ThreadId),
        TocUnloaded(StreamKind, usize, thread::ThreadId),
        FileUnloaded(StreamKind, Option<usize>, bool, thread::ThreadId),
    }

    #[derive(Clone, Default)]
    struct RecordingSink {
        events: Arc<Mutex<Vec<SinkEvent>>>,
    }

    impl StreamRegistrationSink for RecordingSink {
        type Error = Infallible;

        fn collection_map_loaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            map: &CollectionMap,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::MapLoaded(
                context.kind(),
                context.paths().collection_directory.clone(),
                map.collections.len(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn asset_toc_loaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            toc: &AssetToc,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::TocLoaded(
                context.kind(),
                toc.assets.len(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn collection_space_loaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            space: &CollectionSpace,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::SpaceLoaded(
                context.kind(),
                space.collections.len(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn stream_xml_loaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            bytes: &[u8],
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::XmlLoaded(
                context.kind(),
                bytes.to_vec(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn collection_map_unloaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            map: &CollectionMap,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::MapUnloaded(
                context.kind(),
                map.collections.len(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn asset_toc_unloaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            toc: &AssetToc,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::TocUnloaded(
                context.kind(),
                toc.assets.len(),
                thread::current().id(),
            ));
            Ok(())
        }

        fn stream_file_unloaded(
            &mut self,
            context: StreamRegistrationContext<'_>,
            space: Option<&CollectionSpace>,
            had_stream_xml: bool,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(SinkEvent::FileUnloaded(
                context.kind(),
                space.map(|space| space.collections.len()),
                had_stream_xml,
                thread::current().id(),
            ));
            Ok(())
        }
    }

    fn backend(source: Arc<TestSource>) -> MountedStreamBackend<RecordingSink> {
        MountedStreamBackend::with_source(source, RecordingSink::default()).unwrap()
    }

    fn request(index: usize) -> AsyncReadRequest {
        AsyncReadRequest {
            stage: ReadStage::CollectionSpace,
            path: format!("data\\stream\\request-{index}.xss"),
            flags: 0,
            alignment: NATIVE_READ_ALIGNMENT,
        }
    }

    fn wait_for_terminal(
        backend: &mut MountedStreamBackend<RecordingSink>,
        handle: &mut MountedReadHandle,
    ) -> AsyncReadPoll {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let result = backend.poll_read(handle).unwrap();
            if result != AsyncReadPoll::Pending {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for terminal read"
            );
            thread::yield_now();
        }
    }

    fn wait_for_operation_phase(
        backend: &MountedStreamBackend<RecordingSink>,
        path: &str,
        expected: OperationPhase,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let core = &backend.scheduler.core;
        let mut state = core.state.lock().unwrap();
        loop {
            if state
                .operations
                .values()
                .any(|operation| operation.request.path == path && operation.phase == expected)
            {
                return;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for operation phase");
            let (next, timeout) = core.wake.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(
                !timeout.timed_out(),
                "timed out waiting for {path} to reach {expected:?}"
            );
        }
    }

    /// Waits until no operation for `path` remains: the discard path a
    /// worker records asynchronously after a dead handle's retry.
    fn wait_for_no_operation(backend: &MountedStreamBackend<RecordingSink>, path: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let core = &backend.scheduler.core;
        let mut state = core.state.lock().unwrap();
        while state
            .operations
            .values()
            .any(|operation| operation.request.path == path)
        {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for operation discard");
            let (next, timeout) = core.wake.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(
                !timeout.timed_out(),
                "timed out waiting for {path} to be discarded"
            );
        }
    }

    fn wait_for_scheduler_fault(
        backend: &MountedStreamBackend<RecordingSink>,
        expected: MountedReadSchedulerError,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let core = &backend.scheduler.core;
        let mut state = core.state.lock().unwrap();
        while state.fault != Some(expected) {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for scheduler fault");
            let (next, timeout) = core.wake.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(!timeout.timed_out(), "timed out waiting for {expected:?}");
        }
    }

    fn wait_for_admission_count(backend: &MountedStreamBackend<RecordingSink>, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .admission_order
                .len()
                >= expected
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} admissions"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn three_active_leases_and_terminal_completion_do_not_admit_fourth_until_drop() {
        let source = Arc::new(TestSource::default());
        for index in 0..4 {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let mut handles: Vec<_> = (0..4)
            .map(|index| backend.begin_read(request(index)).unwrap())
            .collect();
        source.wait_for_started(3);
        assert_eq!(source.started_paths().len(), 3);

        for index in 0..3 {
            source.release(&request(index).path);
        }
        for handle in handles.iter_mut().take(3) {
            assert!(matches!(
                wait_for_terminal(&mut backend, handle),
                AsyncReadPoll::Complete(_)
            ));
        }
        assert_eq!(source.started_paths().len(), 3);

        drop(handles.remove(0));
        source.wait_for_started(4);
        assert_eq!(source.started_paths()[3], request(3).path);
        source.release_all();
    }

    #[test]
    fn failed_terminal_lease_does_not_admit_fourth_until_handle_drop() {
        let source = Arc::new(TestSource::default());
        for index in 0..4 {
            source.block(&request(index).path);
        }
        source.set_outcome(&request(2).path, TestOutcome::Failed);
        let mut backend = backend(Arc::clone(&source));
        let _first = backend.begin_read(request(0)).unwrap();
        let _second = backend.begin_read(request(1)).unwrap();
        let mut failed = backend.begin_read(request(2)).unwrap();
        let _fourth = backend.begin_read(request(3)).unwrap();
        source.wait_for_started(3);

        source.release(&request(2).path);
        wait_for_operation_phase(&backend, &request(2).path, OperationPhase::Failed);
        assert_eq!(source.started_paths().len(), 3);
        assert_eq!(
            backend.poll_read(&mut failed).unwrap(),
            AsyncReadPoll::Failed
        );
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            MAX_ACTIVE_ASYNC_READS
        );

        drop(failed);
        source.wait_for_path_started(&request(3).path);
        source.release_all();
    }

    #[test]
    fn early_drop_keeps_active_cap_until_in_flight_read_terminates() {
        let source = Arc::new(TestSource::default());
        for index in 0..4 {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let first = backend.begin_read(request(0)).unwrap();
        let _second = backend.begin_read(request(1)).unwrap();
        let _third = backend.begin_read(request(2)).unwrap();
        let _fourth = backend.begin_read(request(3)).unwrap();
        source.wait_for_started(3);

        drop(first);
        thread::yield_now();
        assert_eq!(source.started_paths().len(), 3);
        source.release(&request(0).path);
        source.wait_for_started(4);
        assert!(source.maximum_active() <= MAX_ACTIVE_ASYNC_READS);
        source.release_all();
    }

    /// A `Retry` recorded after its handle dropped discards the operation
    /// and releases its lease instead of re-queueing: no fault, no re-read,
    /// and the scheduler keeps serving later reads. (Inline mode reads only
    /// during a poll, with the handle live, so this is the workers' case.)
    #[test]
    fn retry_after_handle_drop_discards_cleanly() {
        let source = Arc::new(TestSource::default());
        source.block(&request(0).path);
        source.set_outcome(&request(0).path, TestOutcome::NotReady);
        let mut backend = backend(Arc::clone(&source));
        let handle = backend.begin_read(request(0)).unwrap();
        source.wait_for_path_started(&request(0).path);
        drop(handle);
        source.release(&request(0).path);
        // The worker records the discard asynchronously; wait for it so the
        // assertions below observe the settled scheduler, not the race.
        wait_for_no_operation(&backend, &request(0).path);
        let mut fresh = backend.begin_read(request(1)).unwrap();
        assert!(matches!(
            wait_for_terminal(&mut backend, &mut fresh),
            AsyncReadPoll::Complete(_)
        ));
        assert_eq!(
            backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .operations
                .len(),
            1,
            "the dropped retry is gone; only the fresh read remains"
        );
        assert_eq!(
            source.started_paths(),
            vec![request(0).path.clone(), request(1).path.clone()],
            "the retry was not re-attempted after the discard"
        );
    }

    #[test]
    fn dropping_pending_request_preserves_fifo_order_of_survivors() {
        let source = Arc::new(TestSource::default());
        for index in 0..6 {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let mut active: Vec<_> = (0..3)
            .map(|index| backend.begin_read(request(index)).unwrap())
            .collect();
        let dropped_pending = backend.begin_read(request(3)).unwrap();
        let _fifth = backend.begin_read(request(4)).unwrap();
        let _sixth = backend.begin_read(request(5)).unwrap();
        source.wait_for_started(3);

        drop(dropped_pending);
        for index in 0..2 {
            source.release(&request(index).path);
            let mut completed = active.remove(0);
            let result = wait_for_terminal(&mut backend, &mut completed);
            assert!(matches!(result, AsyncReadPoll::Complete(_)));
            drop(completed);
            wait_for_admission_count(&backend, 4 + index);
        }
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().admission_order,
            [1, 2, 3, 5, 6]
        );
        source.release_all();
    }

    #[test]
    fn dropping_admitted_not_read_operation_retains_its_active_lease() {
        let source = Arc::new(TestSource::default());
        let core = Arc::new(SchedulerCore {
            source,
            state: Mutex::new(SchedulerState::new()),
            wake: Condvar::new(),
        });
        let id = 41;
        {
            let mut state = core.state.lock().unwrap();
            state.operations.insert(
                id,
                Operation {
                    request: request(0),
                    phase: OperationPhase::Admitted,
                    bytes: None,
                    handle_alive: true,
                },
            );
            state.ready.push_back(id);
            state.active_leases = 1;
        }
        let handle = MountedReadHandle {
            id,
            core: Arc::clone(&core),
        };

        drop(handle);

        let state = core.state.lock().unwrap();
        let operation = state.operations.get(&id).unwrap();
        assert_eq!(operation.phase, OperationPhase::Admitted);
        assert!(!operation.handle_alive);
        assert_eq!(state.active_leases, 1);
        assert_eq!(state.ready, [id]);
    }

    #[test]
    fn pool_has_exactly_128_owned_operations_and_129th_is_safe_error() {
        let source = Arc::new(TestSource::default());
        for index in 0..ASYNC_OPERATION_CAPACITY {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let handles: Vec<_> = (0..ASYNC_OPERATION_CAPACITY)
            .map(|index| backend.begin_read(request(index)).unwrap())
            .collect();
        let error = backend
            .begin_read(request(ASYNC_OPERATION_CAPACITY))
            .unwrap_err();
        assert!(matches!(
            error,
            MountedStreamBackendError::Scheduler(MountedReadSchedulerError::OperationPoolExhausted)
        ));
        drop(handles);
        source.release_all();
    }

    #[test]
    fn pending_admission_is_fifo() {
        let source = Arc::new(TestSource::default());
        for index in 0..6 {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let mut handles: Vec<_> = (0..6)
            .map(|index| backend.begin_read(request(index)).unwrap())
            .collect();
        source.wait_for_started(3);
        for index in 0..3 {
            source.release(&request(index).path);
            let mut handle = handles.remove(0);
            let result = wait_for_terminal(&mut backend, &mut handle);
            assert!(matches!(result, AsyncReadPoll::Complete(_)));
            drop(handle);
            source.wait_for_started(4 + index);
        }
        let state = backend.scheduler.core.state.lock().unwrap();
        assert_eq!(&state.admission_order[..6], &[1, 2, 3, 4, 5, 6]);
        drop(state);
        source.release_all();
    }

    #[test]
    fn file_failure_is_sticky_but_worker_panic_is_scheduler_error() {
        let failed_source = Arc::new(TestSource::default());
        failed_source.set_outcome(&request(0).path, TestOutcome::Failed);
        let mut failed_backend = backend(Arc::clone(&failed_source));
        let mut failed = failed_backend.begin_read(request(0)).unwrap();
        failed_source.wait_for_started(1);
        assert_eq!(
            wait_for_terminal(&mut failed_backend, &mut failed),
            AsyncReadPoll::Failed
        );
        assert_eq!(
            failed_backend.poll_read(&mut failed).unwrap(),
            AsyncReadPoll::Failed
        );

        let panic_source = Arc::new(TestSource::default());
        panic_source.set_outcome(&request(1).path, TestOutcome::Panic);
        let mut panic_backend = backend(Arc::clone(&panic_source));
        let mut panicked = panic_backend.begin_read(request(1)).unwrap();
        panic_source.wait_for_started(1);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match panic_backend.poll_read(&mut panicked) {
                Err(MountedStreamBackendError::Scheduler(
                    MountedReadSchedulerError::WorkerPanicked,
                )) => break,
                Ok(AsyncReadPoll::Pending) => {
                    assert!(
                        Instant::now() < deadline,
                        "worker panic did not become an error"
                    );
                    thread::yield_now();
                }
                other => panic!("unexpected panic result: {other:?}"),
            }
        }
    }

    #[test]
    fn second_complete_poll_is_error_and_retains_lease_until_drop() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(source);
        let mut handle = backend.begin_read(request(0)).unwrap();
        assert!(matches!(
            wait_for_terminal(&mut backend, &mut handle),
            AsyncReadPoll::Complete(_)
        ));
        let error = backend.poll_read(&mut handle).unwrap_err();
        assert!(matches!(
            error,
            MountedStreamBackendError::Scheduler(
                MountedReadSchedulerError::CompleteResultAlreadyTaken
            )
        ));
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            1
        );
        drop(handle);
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            0
        );
    }

    #[test]
    fn polling_handle_through_foreign_backend_is_explicit_error() {
        let first_source = Arc::new(TestSource::default());
        first_source.block(&request(0).path);
        let mut first = backend(Arc::clone(&first_source));
        let mut second = backend(Arc::new(TestSource::default()));
        let mut handle = first.begin_read(request(0)).unwrap();
        first_source.wait_for_started(1);

        let error = second.poll_read(&mut handle).unwrap_err();
        assert!(matches!(
            error,
            MountedStreamBackendError::Scheduler(MountedReadSchedulerError::ForeignHandle)
        ));

        first_source.release_all();
        let _ = wait_for_terminal(&mut first, &mut handle);
    }

    #[test]
    fn operation_id_wrap_skips_live_collision() {
        let source = Arc::new(TestSource::default());
        for index in 0..4 {
            source.block(&request(index).path);
        }
        let mut backend = backend(Arc::clone(&source));
        let first = backend.begin_read(request(0)).unwrap();
        backend.scheduler.core.state.lock().unwrap().next_id = u64::MAX;
        let maximum = backend.begin_read(request(1)).unwrap();
        let zero = backend.begin_read(request(2)).unwrap();
        let after_collision = backend.begin_read(request(3)).unwrap();

        assert_eq!(first.id, 1);
        assert_eq!(maximum.id, u64::MAX);
        assert_eq!(zero.id, 0);
        assert_eq!(after_collision.id, 2);
        source.release_all();
    }

    #[test]
    fn worker_panic_fault_prevents_later_fifo_admission() {
        let source = Arc::new(TestSource::default());
        for index in 0..4 {
            source.block(&request(index).path);
        }
        source.set_outcome(&request(2).path, TestOutcome::Panic);
        let mut backend = backend(Arc::clone(&source));
        let _first = backend.begin_read(request(0)).unwrap();
        let _second = backend.begin_read(request(1)).unwrap();
        let mut panicked = backend.begin_read(request(2)).unwrap();
        let _fourth = backend.begin_read(request(3)).unwrap();
        source.wait_for_started(3);

        source.release(&request(2).path);
        wait_for_scheduler_fault(&backend, MountedReadSchedulerError::WorkerPanicked);
        assert!(!source.started_paths().contains(&request(3).path));
        assert!(matches!(
            backend.poll_read(&mut panicked),
            Err(MountedStreamBackendError::Scheduler(
                MountedReadSchedulerError::WorkerPanicked
            ))
        ));

        drop(panicked);
        assert!(!source.started_paths().contains(&request(3).path));
        source.release_all();
    }

    #[test]
    fn sink_extraction_waits_for_active_read_then_joins_workers() {
        let source = Arc::new(TestSource::default());
        source.block(&request(0).path);
        let mut backend = backend(Arc::clone(&source));
        let handle = backend.begin_read(request(0)).unwrap();
        source.wait_for_started(1);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let join = thread::spawn(move || {
            entered_tx.send(()).unwrap();
            let sink = backend.into_registration_sink();
            done_tx.send(sink).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

        source.release(&request(0).path);
        let sink = done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(sink.events.lock().unwrap().is_empty());
        join.join().unwrap();
        drop(handle);
    }

    #[test]
    fn missing_path_in_concrete_mount_source_is_a_sticky_file_failure() {
        let mut backend =
            MountedStreamBackend::new(Arc::new(MountSet::new()), RecordingSink::default()).unwrap();
        let mut handle = backend.begin_read(request(0)).unwrap();
        assert_eq!(
            wait_for_terminal(&mut backend, &mut handle),
            AsyncReadPoll::Failed
        );
        assert_eq!(
            backend.poll_read(&mut handle).unwrap(),
            AsyncReadPoll::Failed
        );
    }

    #[test]
    fn poisoned_scheduler_returns_error_instead_of_pending() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(source);
        let core = Arc::clone(&backend.scheduler.core);
        let _ = thread::spawn(move || {
            let _guard = core.state.lock().unwrap();
            panic!("inject poison");
        })
        .join();
        let error = backend.begin_read(request(0)).unwrap_err();
        assert!(matches!(
            error,
            MountedStreamBackendError::Scheduler(MountedReadSchedulerError::Poisoned)
        ));
    }

    #[test]
    fn requests_are_forwarded_bit_exact_and_invalid_inputs_are_rejected() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(Arc::clone(&source));
        let exact = [
            (ReadStage::CollectionMap, "BAM_Pres.xsm", 1),
            (ReadStage::AssetToc, "BAM_Pres.xst", 1),
            (ReadStage::CollectionSpace, "BAM_Pres.xss", 0),
            (ReadStage::StreamXml, "BAM_Pres.xml", 0),
        ]
        .map(|(stage, name, flags)| AsyncReadRequest {
            stage,
            path: format!("data\\content\\world\\{name}"),
            flags,
            alignment: 0x10,
        });
        for (index, expected) in exact.iter().enumerate() {
            let mut handle = backend.begin_read(expected.clone()).unwrap();
            source.wait_for_started(index + 1);
            assert!(matches!(
                wait_for_terminal(&mut backend, &mut handle),
                AsyncReadPoll::Complete(_)
            ));
            drop(handle);
        }
        assert_eq!(source.started_requests(), exact);

        let mut invalid = request(2);
        invalid.path = "world.big|data\\stream\\BAM_Sim.xss".to_owned();
        assert!(matches!(
            backend.begin_read(invalid),
            Err(MountedStreamBackendError::Request(
                MountedReadRequestError::UnsupportedArchiveNamespace
            ))
        ));
        let mut invalid = request(3);
        invalid.flags = 1;
        assert!(matches!(
            backend.begin_read(invalid),
            Err(MountedStreamBackendError::Request(
                MountedReadRequestError::UnexpectedFlags { .. }
            ))
        ));
        let mut invalid = request(4);
        invalid.alignment = 8;
        assert!(matches!(
            backend.begin_read(invalid),
            Err(MountedStreamBackendError::Request(
                MountedReadRequestError::UnexpectedAlignment { .. }
            ))
        ));
        let mut invalid = request(5);
        invalid.path = "bad\0path".to_owned();
        assert!(matches!(
            backend.begin_read(invalid),
            Err(MountedStreamBackendError::Request(
                MountedReadRequestError::InteriorNul
            ))
        ));
        let mut invalid = request(6);
        invalid.path = "x".repeat(NATIVE_PATH_CAPACITY);
        assert!(matches!(
            backend.begin_read(invalid),
            Err(MountedStreamBackendError::Request(
                MountedReadRequestError::PathTooLong { .. }
            ))
        ));
    }

    #[test]
    fn every_registration_callback_is_forwarded_on_caller_thread() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(source);
        let caller = thread::current().id();
        let map = empty_map();
        let toc = empty_toc();
        let space = empty_space();
        let presentation =
            StreamFile::<(), Infallible>::load("world", "BAM", StreamKind::Presentation).unwrap();
        let simulation =
            StreamFile::<(), Infallible>::load("world", "BAM", StreamKind::Simulation).unwrap();
        let texture =
            StreamFile::<(), Infallible>::load("world", "BAM", StreamKind::Texture).unwrap();
        backend
            .collection_map_loaded(presentation.registration_context(), &map)
            .unwrap();
        backend
            .asset_toc_loaded(simulation.registration_context(), &toc)
            .unwrap();
        backend
            .collection_space_loaded(texture.registration_context(), &space)
            .unwrap();
        backend
            .stream_xml_loaded(presentation.registration_context(), b"xml")
            .unwrap();
        backend
            .collection_map_unloaded(presentation.registration_context(), &map)
            .unwrap();
        backend
            .asset_toc_unloaded(simulation.registration_context(), &toc)
            .unwrap();
        backend
            .stream_file_unloaded(texture.registration_context(), Some(&space), true)
            .unwrap();

        let events = backend.registration_sink().events.lock().unwrap();
        assert_eq!(events.len(), 7);
        for event in events.iter() {
            let event_thread = match event {
                SinkEvent::MapLoaded(_, _, _, thread)
                | SinkEvent::TocLoaded(_, _, thread)
                | SinkEvent::SpaceLoaded(_, _, thread)
                | SinkEvent::XmlLoaded(_, _, thread)
                | SinkEvent::MapUnloaded(_, _, thread)
                | SinkEvent::TocUnloaded(_, _, thread)
                | SinkEvent::FileUnloaded(_, _, _, thread) => thread,
            };
            assert_eq!(*event_thread, caller);
        }
    }

    #[test]
    fn integrated_xsm_registration_precedes_xst_worker_start() {
        let source = Arc::new(TestSource::default());
        let xsm_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xsm";
        let xst_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xst";
        source.set_outcome(xsm_path, TestOutcome::Complete(empty_xsm_bytes()));
        source.set_outcome(xst_path, TestOutcome::Complete(empty_xst_bytes()));
        let mut backend = backend(Arc::clone(&source));
        let sink_events = Arc::clone(&backend.registration_sink().events);
        let mut stream = StreamFile::load(
            "data\\content\\world\\stream",
            "BAM",
            StreamKind::Presentation,
        )
        .unwrap();

        stream.poll(&mut backend).unwrap();
        source.wait_for_started(1);
        let deadline = Instant::now() + Duration::from_secs(3);
        while stream.game_stream_state() != GameStreamState::LOADED {
            stream.poll(&mut backend).unwrap();
            if source.started_paths().contains(&xst_path.to_owned()) {
                assert!(
                    sink_events
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|event| matches!(event, SinkEvent::MapLoaded(..))),
                    "XST worker started before XSM registration"
                );
            }
            assert!(Instant::now() < deadline, "stream did not load XSM/XST");
            thread::yield_now();
        }
        source.wait_for_started(2);
        let requests = source.started_requests();
        assert_eq!(requests[0].stage, ReadStage::CollectionMap);
        assert_eq!((requests[0].flags, requests[0].alignment), (1, 0x10));
        assert_eq!(requests[1].stage, ReadStage::AssetToc);
        assert_eq!((requests[1].flags, requests[1].alignment), (1, 0x10));
    }

    #[test]
    fn integrated_failed_xsm_poll_drops_handle_and_admits_queued_fourth() {
        let source = Arc::new(TestSource::default());
        let xsm_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xsm";
        source.set_outcome(xsm_path, TestOutcome::Failed);
        source.block(xsm_path);
        source.block(&request(0).path);
        source.block(&request(1).path);
        source.block(&request(2).path);
        let mut backend = backend(Arc::clone(&source));
        let _first_blocker = backend.begin_read(request(0)).unwrap();
        let _second_blocker = backend.begin_read(request(1)).unwrap();
        let mut stream = StreamFile::load(
            "data\\content\\world\\stream",
            "BAM",
            StreamKind::Presentation,
        )
        .unwrap();
        stream.poll(&mut backend).unwrap();
        source.wait_for_path_started(xsm_path);

        let _queued_fourth = backend.begin_read(request(2)).unwrap();
        assert!(!source.started_paths().contains(&request(2).path));
        source.release(xsm_path);
        wait_for_operation_phase(&backend, xsm_path, OperationPhase::Failed);
        assert!(!source.started_paths().contains(&request(2).path));
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            MAX_ACTIVE_ASYNC_READS
        );

        stream.poll(&mut backend).unwrap();
        assert_eq!(
            stream.reference_read_failure(),
            Some(ReadStage::CollectionMap)
        );
        assert_eq!(stream.game_stream_state(), GameStreamState::LOAD_FAILED);
        source.wait_for_path_started(&request(2).path);
        source.release_all();
    }

    #[test]
    fn integrated_failed_xml_poll_drops_handle_and_admits_queued_fourth() {
        let source = Arc::new(TestSource::default());
        let xsm_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xsm";
        let xst_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xst";
        let xss_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xss";
        let xml_path = "data\\stream\\BAM_Pres.xml";
        source.set_outcome(xsm_path, TestOutcome::Complete(empty_xsm_bytes()));
        source.set_outcome(xst_path, TestOutcome::Complete(empty_xst_bytes()));
        source.set_outcome(xss_path, TestOutcome::Complete(empty_xss_bytes()));
        let mut backend = backend(Arc::clone(&source));
        let mut stream = StreamFile::load(
            "data\\content\\world\\stream",
            "BAM",
            StreamKind::Presentation,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while stream.state() != StreamFileState::LOAD_DONE {
            stream.poll(&mut backend).unwrap();
            assert!(Instant::now() < deadline, "stream did not fully load");
            thread::yield_now();
        }

        source.block(&request(0).path);
        source.block(&request(1).path);
        source.block(&request(2).path);
        source.block(xml_path);
        source.set_outcome(xml_path, TestOutcome::Failed);
        let _first_blocker = backend.begin_read(request(0)).unwrap();
        let _second_blocker = backend.begin_read(request(1)).unwrap();
        stream.begin_stream_xml(&mut backend).unwrap();
        source.wait_for_path_started(xml_path);
        let _queued_fourth = backend.begin_read(request(2)).unwrap();
        assert!(!source.started_paths().contains(&request(2).path));

        source.release(xml_path);
        wait_for_operation_phase(&backend, xml_path, OperationPhase::Failed);
        assert!(!source.started_paths().contains(&request(2).path));
        stream.poll(&mut backend).unwrap();
        assert_eq!(stream.xml_state(), StreamXmlState::LOAD_DONE);
        assert_eq!(stream.reference_read_failure(), None);
        source.wait_for_path_started(&request(2).path);
        source.release_all();
    }

    #[test]
    fn integrated_xst_and_xss_failures_retain_their_active_handle_lease() {
        let xsm_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xsm";
        let xst_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xst";
        let xss_path = "data\\content\\world\\stream\\BAM\\BAM_Pres.xss";

        let toc_source = Arc::new(TestSource::default());
        toc_source.set_outcome(xsm_path, TestOutcome::Complete(empty_xsm_bytes()));
        toc_source.set_outcome(xst_path, TestOutcome::Failed);
        let mut toc_backend = backend(toc_source);
        let mut toc_stream = StreamFile::load(
            "data\\content\\world\\stream",
            "BAM",
            StreamKind::Presentation,
        )
        .unwrap();
        poll_until_reference_failure(&mut toc_stream, &mut toc_backend, ReadStage::AssetToc);
        assert_eq!(toc_stream.game_stream_state().raw(), 3);
        assert_eq!(
            toc_backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .active_leases,
            1
        );
        toc_stream.poll(&mut toc_backend).unwrap();
        assert_eq!(
            toc_backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .active_leases,
            1
        );

        let space_source = Arc::new(TestSource::default());
        space_source.set_outcome(xsm_path, TestOutcome::Complete(empty_xsm_bytes()));
        space_source.set_outcome(xst_path, TestOutcome::Complete(empty_xst_bytes()));
        space_source.set_outcome(xss_path, TestOutcome::Failed);
        let mut space_backend = backend(space_source);
        let mut space_stream = StreamFile::load(
            "data\\content\\world\\stream",
            "BAM",
            StreamKind::Presentation,
        )
        .unwrap();
        poll_until_reference_failure(
            &mut space_stream,
            &mut space_backend,
            ReadStage::CollectionSpace,
        );
        assert_eq!(space_stream.state().raw(), 2);
        assert_eq!(
            space_backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .active_leases,
            1
        );
        space_stream.poll(&mut space_backend).unwrap();
        assert_eq!(
            space_backend
                .scheduler
                .core
                .state
                .lock()
                .unwrap()
                .active_leases,
            1
        );
    }

    fn poll_until_reference_failure(
        stream: &mut StreamFile<MountedReadHandle, MountedStreamBackendError<Infallible>>,
        backend: &mut MountedStreamBackend<RecordingSink>,
        expected: ReadStage,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while stream.reference_read_failure() != Some(expected) {
            stream.poll(backend).unwrap();
            assert!(
                Instant::now() < deadline,
                "stream did not reach the expected reference read failure"
            );
            thread::yield_now();
        }
    }

    fn empty_map() -> CollectionMap {
        CollectionMap {
            version: 3,
            stamp: 0,
            stream_format: 0,
            name_table_offset: 0,
            collections: Vec::new(),
        }
    }

    fn empty_toc() -> AssetToc {
        AssetToc {
            version: 3,
            stamp: 0,
            name_table_offset: 0,
            assets: Vec::new(),
        }
    }

    fn empty_space() -> CollectionSpace {
        CollectionSpace {
            version: 3,
            stamp: 0,
            collections: Vec::new(),
        }
    }

    fn empty_xsm_bytes() -> Vec<u8> {
        let mut bytes = Vec::from(*b"CMAP");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u64.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes
    }

    fn empty_xst_bytes() -> Vec<u8> {
        let mut bytes = Vec::from(*b"ATOC");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u64.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes
    }

    fn empty_xss_bytes() -> Vec<u8> {
        let mut bytes = Vec::from(*b"CSPA");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u64.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes
    }

    #[test]
    fn completion_lease_is_retained_until_handle_drop() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(source);
        let mut handles: Vec<_> = (0..3)
            .map(|index| backend.begin_read(request(index)).unwrap())
            .collect();
        for handle in &mut handles {
            assert!(matches!(
                wait_for_terminal(&mut backend, handle),
                AsyncReadPoll::Complete(_)
            ));
        }
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            MAX_ACTIVE_ASYNC_READS
        );
        drop(handles.pop());
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            2
        );
    }

    #[test]
    fn worker_never_touches_registration_sink() {
        let source = Arc::new(TestSource::default());
        let mut backend = backend(source);
        let mut handle = backend.begin_read(request(0)).unwrap();
        assert!(matches!(
            wait_for_terminal(&mut backend, &mut handle),
            AsyncReadPoll::Complete(_)
        ));
        assert!(
            backend
                .registration_sink()
                .events
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    /// Inline mode's `poll` is the worker: every admitted read runs there,
    /// FIFO, and handles read the same bytes whichever order they poll in.
    #[test]
    fn inline_mode_completes_reads_at_poll_in_fifo_order() {
        let source = Arc::new(TestSource::default());
        let mut backend = inline_backend(Arc::clone(&source));
        let mut first = backend.begin_read(request(0)).unwrap();
        let mut second = backend.begin_read(request(1)).unwrap();
        let mut third = backend.begin_read(request(2)).unwrap();
        // Poll in reverse: the last poll still ran every ready read.
        assert_eq!(
            backend.poll_read(&mut third).unwrap(),
            AsyncReadPoll::Complete(request(2).path.clone().into_bytes())
        );
        assert_eq!(
            backend.poll_read(&mut second).unwrap(),
            AsyncReadPoll::Complete(request(1).path.clone().into_bytes())
        );
        assert_eq!(
            backend.poll_read(&mut first).unwrap(),
            AsyncReadPoll::Complete(request(0).path.clone().into_bytes())
        );
        assert_eq!(
            source.started_paths(),
            vec![
                "data\\stream\\request-0.xss",
                "data\\stream\\request-1.xss",
                "data\\stream\\request-2.xss"
            ]
        );
    }

    /// Admission is the workers' own: three active leases, the fourth waits
    /// for a handle drop, then completes — with no thread involved.
    #[test]
    fn inline_mode_keeps_the_three_active_admission_cap() {
        let source = Arc::new(TestSource::default());
        let mut backend = inline_backend(Arc::clone(&source));
        let mut first = backend.begin_read(request(0)).unwrap();
        let mut second = backend.begin_read(request(1)).unwrap();
        let mut third = backend.begin_read(request(2)).unwrap();
        let mut fourth = backend.begin_read(request(3)).unwrap();
        for handle in [&mut first, &mut second, &mut third] {
            assert!(matches!(
                backend.poll_read(handle).unwrap(),
                AsyncReadPoll::Complete(_)
            ));
        }
        // Three completed reads hold their leases until their handles drop.
        assert_eq!(
            backend.poll_read(&mut fourth).unwrap(),
            AsyncReadPoll::Pending
        );
        assert_eq!(source.started_paths().len(), 3);
        drop(first);
        assert!(matches!(
            backend.poll_read(&mut fourth).unwrap(),
            AsyncReadPoll::Complete(_)
        ));
        assert_eq!(source.started_paths().len(), 4);
    }

    /// The 128-operation pool is the workers' own too.
    #[test]
    fn inline_mode_exhausts_the_operation_pool_at_capacity() {
        let source = Arc::new(TestSource::default());
        let mut backend = inline_backend(Arc::clone(&source));
        let mut handles = Vec::new();
        for index in 0..ASYNC_OPERATION_CAPACITY {
            handles.push(backend.begin_read(request(index % 8)).unwrap());
        }
        let overflow = backend.begin_read(request(9));
        assert!(
            matches!(
                overflow,
                Err(MountedStreamBackendError::Scheduler(
                    MountedReadSchedulerError::OperationPoolExhausted
                ))
            ),
            "the 129th begin exhausts the pool"
        );
    }

    /// A dropped pending handle is discarded without ever reading.
    #[test]
    fn inline_mode_drops_pending_without_reading() {
        let source = Arc::new(TestSource::default());
        let mut backend = inline_backend(Arc::clone(&source));
        let _first = backend.begin_read(request(0)).unwrap();
        let _second = backend.begin_read(request(1)).unwrap();
        let _third = backend.begin_read(request(2)).unwrap();
        let pending = backend.begin_read(request(3)).unwrap();
        drop(pending);
        // Nothing polls; nothing was read at all in inline mode.
        assert!(source.started_paths().is_empty());
    }

    /// A source failure reads as the workers' own `Failed`.
    #[test]
    fn inline_mode_reports_source_failures() {
        let source = Arc::new(TestSource::default());
        source.set_outcome(&request(0).path, TestOutcome::Failed);
        let mut backend = inline_backend(Arc::clone(&source));
        let mut handle = backend.begin_read(request(0)).unwrap();
        assert_eq!(
            backend.poll_read(&mut handle).unwrap(),
            AsyncReadPoll::Failed
        );
    }

    /// A source panic latches the scheduler fault, as the worker's
    /// catch_unwind does.
    #[test]
    fn inline_mode_latches_a_panic_fault() {
        let source = Arc::new(TestSource::default());
        source.set_outcome(&request(0).path, TestOutcome::Panic);
        let mut backend = inline_backend(Arc::clone(&source));
        let mut handle = backend.begin_read(request(0)).unwrap();
        let _ = backend.poll_read(&mut handle);
        // The fault also refuses new work, as it does in worker mode.
        let next = backend.begin_read(request(1));
        assert!(matches!(
            next,
            Err(MountedStreamBackendError::Scheduler(
                MountedReadSchedulerError::WorkerPanicked
            ))
        ));
    }

    /// A not-ready source keeps the poll `Pending`; once the span (here:
    /// the outcome) is ready, the same read completes with the right bytes.
    #[test]
    fn inline_mode_retries_a_not_ready_source_to_completion() {
        let source = Arc::new(TestSource::default());
        source.set_outcome(&request(0).path, TestOutcome::NotReady);
        let mut backend = inline_backend(Arc::clone(&source));
        let mut handle = backend.begin_read(request(0)).unwrap();
        for attempts in 1..=2 {
            assert_eq!(
                backend.poll_read(&mut handle).unwrap(),
                AsyncReadPoll::Pending,
                "a retry is not a completion and not a failure"
            );
            assert_eq!(
                source.started_paths().len(),
                attempts,
                "each poll re-attempted the read exactly once"
            );
        }
        source.set_outcome(
            &request(0).path,
            TestOutcome::Complete(b"resident!".to_vec()),
        );
        assert_eq!(
            backend.poll_read(&mut handle).unwrap(),
            AsyncReadPoll::Complete(b"resident!".to_vec())
        );
        assert_eq!(source.started_paths().len(), 3);
    }

    /// A retry holds its admission lease: while the not-ready read keeps
    /// re-queueing, the fourth begin stays unadmitted, and only a real
    /// completion's handle drop frees the lease it needs.
    #[test]
    fn inline_mode_retry_holds_the_admission_lease() {
        let source = Arc::new(TestSource::default());
        source.set_outcome(&request(0).path, TestOutcome::NotReady);
        let mut backend = inline_backend(Arc::clone(&source));
        let mut first = backend.begin_read(request(0)).unwrap();
        let second = backend.begin_read(request(1)).unwrap();
        let _third = backend.begin_read(request(2)).unwrap();
        let mut fourth = backend.begin_read(request(3)).unwrap();
        // One drain: request-0 retries, request-1 and request-2 complete.
        assert_eq!(
            backend.poll_read(&mut fourth).unwrap(),
            AsyncReadPoll::Pending
        );
        assert_eq!(source.started_paths().len(), 3);
        // Every later poll re-attempts only the retry; its lease never
        // releases, so the fourth begin keeps waiting.
        for _ in 0..2 {
            assert_eq!(
                backend.poll_read(&mut first).unwrap(),
                AsyncReadPoll::Pending
            );
        }
        assert_eq!(
            source.started_paths().len(),
            5,
            "request-0 attempted three times, requests 1 and 2 once each"
        );
        assert_eq!(
            backend.scheduler.core.state.lock().unwrap().active_leases,
            MAX_ACTIVE_ASYNC_READS,
            "the retrying operation still holds one of the three leases"
        );
        assert_eq!(
            backend.poll_read(&mut fourth).unwrap(),
            AsyncReadPoll::Pending,
            "no retry freed a lease for the fourth begin"
        );
        drop(second);
        assert!(matches!(
            backend.poll_read(&mut fourth).unwrap(),
            AsyncReadPoll::Complete(_)
        ));
        assert_eq!(
            source.started_paths().len(),
            8,
            "the last two drains re-attempted the retry, then read request-3"
        );
    }

    /// The inline scheduler through the same backend harness as the workers.
    fn inline_backend(source: Arc<TestSource>) -> MountedStreamBackend<RecordingSink> {
        MountedStreamBackend {
            scheduler: ReadScheduler::inline(source),
            sink: RecordingSink::default(),
        }
    }
}
