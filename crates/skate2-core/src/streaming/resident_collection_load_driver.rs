//! Concrete ownership composition for one resident collection load.
//!
//! This module joins the mount-backed producer and the recovered collection
//! pump to an owned, deferred asset-construction FIFO. Native resource
//! constructors remain behind [`CollectionAssetBuilder`].

use crate::{
    collection_load_pump::{
        CollectionAssetMetadata, CollectionDecompressionQueue, CollectionDecompressionSubmission,
        CollectionLoadPump, CollectionLoadPumpError, CollectionLoadSlot, CollectionLoadUpdate,
        CollectionStreamFormat, LoadedAssetCollection,
    },
    collection_resident_streamer::{ResidentCollectionLoadDriver, ResidentDecompressionKick},
    mounted_collection_producer::{
        CollectionLogicalRangeSource, MountedCollectionBlockView, MountedCollectionChunk,
        MountedCollectionChunkStream, MountedCollectionProducer, MountedCollectionProducerError,
        MountedCollectionProducerPoll,
    },
};
use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Maximum number of native decompression requests copied into one worker
/// snapshot at `cAssetStreamer::DecompressThread` (`0x82d5adb0`).
pub const DECOMPRESSION_SNAPSHOT_LIMIT: usize = 32;

/// Source-byte gate tested before every request after the first in a worker
/// slice at `cAssetStreamer::DecompressThread` (`0x82d5afb8`).
pub const DECOMPRESSION_SOURCE_BUDGET: u32 = 0x0004_0000;

/// Exact constructor family selected from the raw native stream-format word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionAssetBuildMode {
    Plain,
    Compressed,
    CompressedArena,
    CompressedChunkArena,
}

impl CollectionAssetBuildMode {
    /// Reconstructs the dispatch in `cAssetStreamer::DecompressThread` at
    /// `0x82d5afdc..0x82d5b030`.
    #[must_use]
    pub const fn from_raw(format: CollectionStreamFormat) -> Self {
        match format.0 {
            1 => Self::Compressed,
            2 => Self::CompressedArena,
            3 => Self::CompressedChunkArena,
            _ => Self::Plain,
        }
    }
}

/// Fully owned copy of one borrowed pump submission.
///
/// The block view retains bytes but has no stream-release authority. The pump
/// ring remains the sole owner of the corresponding `MountedCollectionChunk`.
pub struct OwnedDecompressionJob<I> {
    pub slot: CollectionLoadSlot,
    pub asset_id: u64,
    pub asset_info: I,
    pub block: MountedCollectionBlockView,
    pub data_offset: u32,
    pub stored_size: u32,
    pub format: CollectionStreamFormat,
}

impl<I: Clone> Clone for OwnedDecompressionJob<I> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            asset_id: self.asset_id,
            asset_info: self.asset_info.clone(),
            block: self.block.clone(),
            data_offset: self.data_offset,
            stored_size: self.stored_size,
            format: self.format,
        }
    }
}

impl<I> OwnedDecompressionJob<I> {
    /// The constructor input, or `None` when the payload lies outside its block.
    #[must_use]
    pub fn build_input(&self) -> Option<CollectionAssetBuildInput<'_, I>> {
        Some(CollectionAssetBuildInput {
            asset_id: self.asset_id,
            asset_info: &self.asset_info,
            block: &self.block,
            payload: self.block.payload_bytes(self.data_offset, self.stored_size)?,
            stored_size: self.stored_size,
            raw_format: self.format,
            mode: CollectionAssetBuildMode::from_raw(self.format),
        })
    }
}

impl<I: fmt::Debug> fmt::Debug for OwnedDecompressionJob<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedDecompressionJob")
            .field("slot", &self.slot)
            .field("asset_id", &format_args!("{:#018x}", self.asset_id))
            .field("asset_info", &self.asset_info)
            .field("block", &self.block)
            .field("data_offset", &self.data_offset)
            .field("stored_size", &self.stored_size)
            .field("format", &self.format)
            .finish()
    }
}

/// Owned FIFO corresponding to native `mRequestVector`.
///
/// Native `AddDecompressRequest` at `0x82d5c280` copies a 40-byte request into
/// its vector. This host representation instead clones typed metadata and an
/// Arc-backed, non-releasing block view at submission time.
pub struct OwnedCollectionDecompressionQueue<I, A> {
    jobs: VecDeque<OwnedDecompressionJob<I>>,
    asset: PhantomData<fn() -> A>,
}

impl<I, A> Default for OwnedCollectionDecompressionQueue<I, A> {
    fn default() -> Self {
        Self {
            jobs: VecDeque::new(),
            asset: PhantomData,
        }
    }
}

impl<I, A> OwnedCollectionDecompressionQueue<I, A> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    #[must_use]
    pub fn front(&self) -> Option<&OwnedDecompressionJob<I>> {
        self.jobs.front()
    }

    fn jobs(&self) -> impl Iterator<Item = &OwnedDecompressionJob<I>> {
        self.jobs.iter()
    }

    fn take_snapshot(&mut self) -> VecDeque<OwnedDecompressionJob<I>> {
        let count = self.jobs.len().min(DECOMPRESSION_SNAPSHOT_LIMIT);
        self.jobs.drain(..count).collect()
    }

    fn prepend(&mut self, mut jobs: VecDeque<OwnedDecompressionJob<I>>) {
        while let Some(job) = jobs.pop_back() {
            self.jobs.push_front(job);
        }
    }
}

impl<I: Clone, A> CollectionDecompressionQueue<MountedCollectionChunk, I>
    for OwnedCollectionDecompressionQueue<I, A>
{
    type Asset = A;

    fn submit(
        &mut self,
        submission: CollectionDecompressionSubmission<'_, MountedCollectionChunk, I>,
    ) {
        self.jobs.push_back(OwnedDecompressionJob {
            slot: submission.slot,
            asset_id: submission.asset_id,
            asset_info: submission.asset_info.clone(),
            block: submission.chunk.owned_block_view(),
            data_offset: submission.data_offset,
            stored_size: submission.stored_size,
            format: submission.format,
        });
    }
}

/// Borrowed, range-checked input to an opaque resource constructor.
pub struct CollectionAssetBuildInput<'a, I> {
    pub asset_id: u64,
    pub asset_info: &'a I,
    pub block: &'a MountedCollectionBlockView,
    pub payload: &'a [u8],
    pub stored_size: u32,
    pub raw_format: CollectionStreamFormat,
    pub mode: CollectionAssetBuildMode,
}

impl<I> Clone for CollectionAssetBuildInput<'_, I> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I> Copy for CollectionAssetBuildInput<'_, I> {}

/// Opaque native asset construction/decompression boundary.
pub trait CollectionAssetBuilder<I> {
    type Asset;
    type Error: Error + Send + Sync + 'static;

    /// Constructs one asset in FIFO order.
    ///
    /// `Ok(None)` represents the native callback receiving a null asset and
    /// still completing the request slot. Actual decode/resource algorithms
    /// are intentionally outside this reconstruction.
    ///
    /// # Errors
    ///
    /// Reports a typed host construction failure. The driver retains the
    /// failed job and all later jobs without completing its pump slot.
    fn build(
        &mut self,
        input: CollectionAssetBuildInput<'_, I>,
    ) -> Result<Option<Self::Asset>, Self::Error>;

    /// Starts constructing a queued job away from its owner, as the native
    /// decompression thread did, or `None` to construct it at the drain.
    /// Only a builder whose output depends on the job alone may prefetch; the
    /// driver still completes every slot at its ordinary drain, in FIFO order.
    fn prefetch(&self, _job: &OwnedDecompressionJob<I>) -> Option<Prefetch<Self::Asset, Self::Error>> {
        None
    }
}

/// One queued job's construction running on its own worker thread.
pub struct Prefetch<A, E>(crate::host_thread::WorkerHandle<Result<Option<A>, E>>);

impl<A: Send + 'static, E: Send + 'static> Prefetch<A, E> {
    /// `None` when no worker could be started; the job then builds in place.
    pub fn spawn(build: impl FnOnce() -> Result<Option<A>, E> + Send + 'static) -> Option<Self> {
        crate::host_thread::spawn_worker("sk8 arena decode", build)
            .ok()
            .map(Self)
    }
}

impl<A, E> Prefetch<A, E> {
    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    fn wait(self) -> Result<Option<A>, E> {
        self.0.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

/// Shared, coalescing `StartDecompress` request.
///
/// The native single evidenced caller path changes a dirty word and posts only
/// when it had been zero (`0x82d54b68`). `AtomicBool` additionally makes
/// concurrent host callers safe, so concurrent behavior is a high-confidence
/// policy rather than a claimed native synchronization primitive.
#[derive(Clone, Debug, Default)]
pub struct CollectionDecompressionWake {
    pending: Arc<AtomicBool>,
}

impl CollectionDecompressionWake {
    /// Coalesces this request with an already-pending wake.
    ///
    /// Returns `true` only when this call creates a newly pending wake.
    #[must_use]
    pub fn request(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

impl<E> ResidentDecompressionKick<E> for CollectionDecompressionWake {
    fn start_decompress(&mut self) -> Result<(), E> {
        let _ = self.request();
        Ok(())
    }
}

/// Work observed during one deterministic host decompression wake.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecompressionWakeProgress {
    pub snapshots: u32,
    pub source_slices: u32,
    pub jobs_completed: u32,
}

/// Work observed during one serial driver update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResidentCollectionDriverUpdate {
    pub decompression: DecompressionWakeProgress,
    pub producer: MountedCollectionProducerPoll,
    pub pump: CollectionLoadUpdate,
}

/// First-fault errors from the concrete ownership composition.
#[derive(Debug)]
pub enum MountedResidentCollectionLoadDriverError<SE, ME, BE> {
    ZeroAssetRequest,
    Producer(Arc<MountedCollectionProducerError<SE>>),
    Pump(Arc<CollectionLoadPumpError<ME>>),
    PayloadOutsideBlock {
        slot: CollectionLoadSlot,
        asset_id: u64,
        data_offset: u32,
        stored_size: u32,
        block_size: usize,
    },
    Builder {
        slot: CollectionLoadSlot,
        asset_id: u64,
        source: Arc<BE>,
    },
}

impl<SE: fmt::Display, ME: fmt::Display, BE: fmt::Display> fmt::Display
    for MountedResidentCollectionLoadDriverError<SE, ME, BE>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAssetRequest => formatter.write_str(
                "zero-asset collections require the resident streamer's separate deferred zero driver",
            ),
            Self::Producer(source) => write!(formatter, "collection producer failed: {source}"),
            Self::Pump(source) => write!(formatter, "collection pump failed: {source}"),
            Self::PayloadOutsideBlock {
                slot,
                asset_id,
                data_offset,
                stored_size,
                block_size,
            } => write!(
                formatter,
                "asset {asset_id:#018x} slot {}/{} payload {data_offset:#x}+{stored_size:#x} is outside its {block_size:#x}-byte block",
                slot.index(),
                slot.reservation()
            ),
            Self::Builder {
                slot,
                asset_id,
                source,
            } => write!(
                formatter,
                "asset {asset_id:#018x} slot {}/{} construction failed: {source}",
                slot.index(),
                slot.reservation()
            ),
        }
    }
}

impl<SE, ME, BE> Error for MountedResidentCollectionLoadDriverError<SE, ME, BE>
where
    SE: Error + Send + Sync + 'static,
    ME: Error + Send + Sync + 'static,
    BE: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Producer(source) => Some(source.as_ref()),
            Self::Pump(source) => Some(source.as_ref()),
            Self::Builder { source, .. } => Some(source.as_ref()),
            Self::ZeroAssetRequest | Self::PayloadOutsideBlock { .. } => None,
        }
    }
}

type DriverQueue<M, B> = OwnedCollectionDecompressionQueue<
    <M as CollectionAssetMetadata>::AssetInfo,
    <B as CollectionAssetBuilder<<M as CollectionAssetMetadata>::AssetInfo>>::Asset,
>;

type DriverPump<M, C, B> =
    CollectionLoadPump<MountedCollectionChunkStream, M, DriverQueue<M, B>, C>;

type DriverError<R, M, B> = MountedResidentCollectionLoadDriverError<
    <R as CollectionLogicalRangeSource>::Error,
    <M as CollectionAssetMetadata>::Error,
    <B as CollectionAssetBuilder<<M as CollectionAssetMetadata>::AssetInfo>>::Error,
>;

/// One queued job's identity and its construction started at submission.
type PrefetchedJob<A, E> = (CollectionLoadSlot, u64, Option<Prefetch<A, E>>);

/// Immutable request fields used to pair the producer and collection pump.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountedResidentCollectionLoadSpec {
    pub path: Arc<str>,
    pub payload_offset: u64,
    pub request_id: i32,
    pub target_assets: i32,
    pub format: CollectionStreamFormat,
}

/// Serial PC composition of the recovered producer, request pump, and worker
/// FIFO.
///
/// Each update first drains a wake requested after the prior update, then
/// performs one producer stage and one pump update. Native worker scheduling
/// and `ThreadSleep(1)` calls are not reproduced; this deterministic cadence
/// is a high-confidence host policy. Pump admission and worker FIFO/budget
/// ordering remain exact within that cadence.
pub struct MountedResidentCollectionLoadDriver<R, M, C, B>
where
    R: CollectionLogicalRangeSource,
    M: CollectionAssetMetadata,
    M::AssetInfo: Clone,
    B: CollectionAssetBuilder<M::AssetInfo>,
    C: LoadedAssetCollection<B::Asset>,
{
    producer: MountedCollectionProducer<R>,
    pump: DriverPump<M, C, B>,
    builder: B,
    wake: CollectionDecompressionWake,
    terminal_error: Option<Arc<DriverError<R, M, B>>>,
    /// Parallel to the decompression FIFO: one entry per queued job.
    prefetched: VecDeque<PrefetchedJob<B::Asset, B::Error>>,
}

impl<R, M, C, B> MountedResidentCollectionLoadDriver<R, M, C, B>
where
    R: CollectionLogicalRangeSource,
    M: CollectionAssetMetadata,
    M::AssetInfo: Clone,
    M::Error: Error + Send + Sync + 'static,
    B: CollectionAssetBuilder<M::AssetInfo>,
    C: LoadedAssetCollection<B::Asset>,
{
    /// Creates the paired producer/pump ownership without reading the source.
    ///
    /// # Errors
    ///
    /// Returns the producer's typed path, offset, or request-sentinel error.
    pub fn new(
        source: Arc<R>,
        spec: MountedResidentCollectionLoadSpec,
        metadata: M,
        collection: C,
        builder: B,
        wake: CollectionDecompressionWake,
    ) -> Result<Self, Arc<DriverError<R, M, B>>> {
        if spec.target_assets == 0 {
            return Err(Arc::new(
                MountedResidentCollectionLoadDriverError::ZeroAssetRequest,
            ));
        }
        let (producer, stream) =
            MountedCollectionProducer::new(source, spec.path, spec.payload_offset, spec.request_id)
                .map_err(|error| {
                    Arc::new(MountedResidentCollectionLoadDriverError::Producer(error))
                })?;
        let pump = CollectionLoadPump::new(
            stream,
            metadata,
            OwnedCollectionDecompressionQueue::default(),
            collection,
            spec.request_id,
            spec.target_assets,
            spec.format,
        );
        Ok(Self {
            producer,
            pump,
            builder,
            wake,
            terminal_error: None,
            prefetched: VecDeque::new(),
        })
    }

    #[must_use]
    pub const fn pump(&self) -> &DriverPump<M, C, B> {
        &self.pump
    }

    #[must_use]
    pub const fn builder(&self) -> &B {
        &self.builder
    }

    #[must_use]
    pub const fn wake(&self) -> &CollectionDecompressionWake {
        &self.wake
    }

    #[must_use]
    pub fn terminal_error(&self) -> Option<&Arc<DriverError<R, M, B>>> {
        self.terminal_error.as_ref()
    }

    /// Advances the deterministic serial composition once.
    ///
    /// # Errors
    ///
    /// Latches and replays by `Arc` identity the first producer, pump,
    /// payload-range, or builder failure. A failed owned job and every later
    /// job remain queued, and its pump slot remains retained.
    pub fn update_one_tick(
        &mut self,
    ) -> Result<ResidentCollectionDriverUpdate, Arc<DriverError<R, M, B>>> {
        self.check_terminal()?;
        let decompression = self.drain_prior_wake()?;
        let producer = match self.producer.poll_parser(&mut self.pump) {
            Ok(progress) => progress,
            Err(error) => {
                return self.latch(MountedResidentCollectionLoadDriverError::Producer(error));
            }
        };
        let pump = match self.pump.update() {
            Ok(progress) => progress,
            Err(error) => {
                return self.latch(MountedResidentCollectionLoadDriverError::Pump(error));
            }
        };
        self.prefetch_submitted();
        Ok(ResidentCollectionDriverUpdate {
            decompression,
            producer,
            pump,
        })
    }

    fn drain_prior_wake(&mut self) -> Result<DecompressionWakeProgress, Arc<DriverError<R, M, B>>> {
        let mut progress = DecompressionWakeProgress::default();
        if !self.wake.take() {
            return Ok(progress);
        }

        loop {
            let mut snapshot = self.pump.decompression_mut().take_snapshot();
            if snapshot.is_empty() {
                break;
            }
            progress.snapshots = progress.snapshots.wrapping_add(1);
            let mut prefetched = self
                .prefetched
                .drain(..snapshot.len().min(self.prefetched.len()))
                .collect::<VecDeque<_>>();

            while !snapshot.is_empty() {
                progress.source_slices = progress.source_slices.wrapping_add(1);
                let mut source_bytes = 0_u32;
                let mut first = true;
                while first || source_bytes < DECOMPRESSION_SOURCE_BUDGET {
                    let Some(job) = snapshot.pop_front() else {
                        break;
                    };
                    let decoding = prefetched.front().is_some_and(|(slot, asset_id, prefetch)| {
                        *slot == job.slot
                            && *asset_id == job.asset_id
                            && prefetch.as_ref().is_some_and(|prefetch| !prefetch.is_finished())
                    });
                    if decoding {
                        // As the native decompression thread's completions arrive: a job still
                        // decoding and the jobs behind it complete at a later update, in order.
                        snapshot.push_front(job);
                        self.pump.decompression_mut().prepend(snapshot);
                        while let Some(entry) = prefetched.pop_back() {
                            self.prefetched.push_front(entry);
                        }
                        let _ = self.wake.request();
                        return Ok(progress);
                    }
                    let stored_size = job.stored_size;
                    let completed = match prefetched.pop_front() {
                        Some((slot, asset_id, Some(prefetch)))
                            if slot == job.slot && asset_id == job.asset_id =>
                        {
                            prefetch
                                .wait()
                                .map_err(|source| Self::builder_error(&job, source))
                                .and_then(|asset| self.complete(&job, asset))
                        }
                        _ => self.build_and_complete(&job),
                    };
                    if let Err(error) = completed {
                        snapshot.push_front(job);
                        self.pump.decompression_mut().prepend(snapshot);
                        return self.latch(error);
                    }
                    source_bytes = source_bytes.wrapping_add(stored_size);
                    progress.jobs_completed = progress.jobs_completed.wrapping_add(1);
                    first = false;
                }
            }
        }
        Ok(progress)
    }

    /// Starts each newly submitted job's construction; the drain after the
    /// next wake still completes it, in FIFO order.
    fn prefetch_submitted(&mut self) {
        let queued = self.pump.decompression().len();
        if self.prefetched.len() > queued {
            self.prefetched.clear();
        }
        let started = self.prefetched.len();
        for job in self.pump.decompression().jobs().skip(started) {
            self.prefetched
                .push_back((job.slot, job.asset_id, self.builder.prefetch(job)));
        }
    }

    fn build_input(
        job: &OwnedDecompressionJob<M::AssetInfo>,
    ) -> Result<CollectionAssetBuildInput<'_, M::AssetInfo>, DriverError<R, M, B>> {
        job.build_input().ok_or_else(|| {
            MountedResidentCollectionLoadDriverError::PayloadOutsideBlock {
                slot: job.slot,
                asset_id: job.asset_id,
                data_offset: job.data_offset,
                stored_size: job.stored_size,
                block_size: job.block.bytes().len(),
            }
        })
    }

    fn builder_error(
        job: &OwnedDecompressionJob<M::AssetInfo>,
        source: B::Error,
    ) -> DriverError<R, M, B> {
        MountedResidentCollectionLoadDriverError::Builder {
            slot: job.slot,
            asset_id: job.asset_id,
            source: Arc::new(source),
        }
    }

    fn build_and_complete(
        &mut self,
        job: &OwnedDecompressionJob<M::AssetInfo>,
    ) -> Result<(), DriverError<R, M, B>> {
        let asset = self
            .builder
            .build(Self::build_input(job)?)
            .map_err(|source| Self::builder_error(job, source))?;
        self.complete(job, asset)
    }

    fn complete(
        &mut self,
        job: &OwnedDecompressionJob<M::AssetInfo>,
        asset: Option<B::Asset>,
    ) -> Result<(), DriverError<R, M, B>> {
        self.pump
            .complete_slot(job.slot, asset)
            .map_err(MountedResidentCollectionLoadDriverError::Pump)
    }

    fn check_terminal(&self) -> Result<(), Arc<DriverError<R, M, B>>> {
        match &self.terminal_error {
            Some(error) => Err(Arc::clone(error)),
            None => Ok(()),
        }
    }

    fn latch<T>(&mut self, error: DriverError<R, M, B>) -> Result<T, Arc<DriverError<R, M, B>>> {
        let error = Arc::new(error);
        self.terminal_error = Some(Arc::clone(&error));
        Err(error)
    }
}

impl<R, M, C, B> ResidentCollectionLoadDriver<Arc<DriverError<R, M, B>>>
    for MountedResidentCollectionLoadDriver<R, M, C, B>
where
    R: CollectionLogicalRangeSource,
    M: CollectionAssetMetadata,
    M::AssetInfo: Clone,
    M::Error: Error + Send + Sync + 'static,
    B: CollectionAssetBuilder<M::AssetInfo>,
    C: LoadedAssetCollection<B::Asset>,
{
    fn update(&mut self) -> Result<(), Arc<DriverError<R, M, B>>> {
        self.update_one_tick().map(|_| ())
    }

    fn is_complete(&self) -> bool {
        self.pump.is_complete()
    }

    fn highest_stored_block(&self) -> u32 {
        self.pump.highest_stored_block()
    }

    fn loaded_size(&self) -> u32 {
        self.pump.loaded_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skate2_formats::mount::{LogicalEntryRange, LogicalEntryStorage};
    use std::{
        collections::HashSet,
        convert::Infallible,
        ops::Range,
        sync::{Mutex, MutexGuard},
        thread,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct SourceError;

    impl fmt::Display for SourceError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("source failure")
        }
    }

    impl Error for SourceError {}

    struct MemorySource {
        bytes: Arc<[u8]>,
        calls: Mutex<Vec<Range<u64>>>,
    }

    impl MemorySource {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes: Arc::from(bytes),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl CollectionLogicalRangeSource for MemorySource {
        type Error = SourceError;

        fn read_range(
            &self,
            _path: &str,
            range: Range<u64>,
        ) -> Result<LogicalEntryRange, Self::Error> {
            lock_recover(&self.calls).push(range.clone());
            let bytes = if range.is_empty() {
                Vec::new()
            } else {
                self.bytes
                    [usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()]
                    .to_vec()
            };
            Ok(LogicalEntryRange {
                bytes,
                logical_len: u64::try_from(self.bytes.len()).unwrap(),
                storage: LogicalEntryStorage::Raw,
            })
        }
    }

    #[derive(Clone, Debug)]
    struct TestAssetInfo {
        id: u64,
        identity: Arc<()>,
    }

    struct TestMetadata {
        sizes: [u32; 3],
        identity: Arc<()>,
        missing: HashSet<u64>,
    }

    impl CollectionAssetMetadata for TestMetadata {
        type AssetInfo = TestAssetInfo;
        type Error = Infallible;

        fn find_asset_info(
            &mut self,
            asset_id: u64,
        ) -> Result<Option<Self::AssetInfo>, Self::Error> {
            Ok((!self.missing.contains(&asset_id)).then(|| TestAssetInfo {
                id: asset_id,
                identity: Arc::clone(&self.identity),
            }))
        }

        fn first_three_resource_sizes(&mut self, _info: &Self::AssetInfo) -> [u32; 3] {
            self.sizes
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct BuildError(u64);

    impl fmt::Display for BuildError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "failed asset {:#018x}", self.0)
        }
    }

    impl Error for BuildError {}

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event {
        Build {
            id: u64,
            info_id: u64,
            mode: CollectionAssetBuildMode,
            raw_format: i32,
            payload_first: u8,
            sequence: u64,
        },
        Add(u64),
    }

    struct TestBuilder {
        events: Arc<Mutex<Vec<Event>>>,
        fail_on: Option<u64>,
        none_on: HashSet<u64>,
    }

    impl CollectionAssetBuilder<TestAssetInfo> for TestBuilder {
        type Asset = u64;
        type Error = BuildError;

        fn build(
            &mut self,
            input: CollectionAssetBuildInput<'_, TestAssetInfo>,
        ) -> Result<Option<Self::Asset>, Self::Error> {
            lock_recover(&self.events).push(Event::Build {
                id: input.asset_id,
                info_id: input.asset_info.id,
                mode: input.mode,
                raw_format: input.raw_format.0,
                payload_first: input.payload.first().copied().unwrap_or_default(),
                sequence: input.block.sequence(),
            });
            if self.fail_on == Some(input.asset_id) {
                return Err(BuildError(input.asset_id));
            }
            Ok((!self.none_on.contains(&input.asset_id)).then_some(input.asset_id))
        }
    }

    struct TestCollection {
        events: Arc<Mutex<Vec<Event>>>,
        assets: Arc<Mutex<Vec<u64>>>,
    }

    impl LoadedAssetCollection<u64> for TestCollection {
        fn add_asset(&mut self, asset: u64) {
            lock_recover(&self.events).push(Event::Add(asset));
            lock_recover(&self.assets).push(asset);
        }
    }

    type TestDriver = MountedResidentCollectionLoadDriver<
        MemorySource,
        TestMetadata,
        TestCollection,
        TestBuilder,
    >;

    struct DriverHarness {
        driver: TestDriver,
        wake: CollectionDecompressionWake,
        events: Arc<Mutex<Vec<Event>>>,
        assets: Arc<Mutex<Vec<u64>>>,
        metadata_identity: Arc<()>,
    }

    fn record(id: u64, stored_size: u32, data_offset: u32, stride: u32, fill: u8) -> Vec<u8> {
        let mut bytes = vec![fill; usize::try_from(stride).unwrap()];
        bytes[0..8].copy_from_slice(&id.to_be_bytes());
        bytes[8..12].copy_from_slice(&stored_size.to_be_bytes());
        bytes[12..16].copy_from_slice(&data_offset.to_be_bytes());
        bytes[16..20].copy_from_slice(&stride.to_be_bytes());
        bytes[20..24].fill(0);
        bytes
    }

    fn harness(
        specs: &[(u64, u32, u32, u32, u8)],
        format: i32,
        fail_on: Option<u64>,
        none_on: &[u64],
    ) -> DriverHarness {
        harness_with_target(
            specs,
            i32::try_from(specs.len()).unwrap(),
            format,
            fail_on,
            none_on,
        )
    }

    fn harness_with_target(
        specs: &[(u64, u32, u32, u32, u8)],
        target_assets: i32,
        format: i32,
        fail_on: Option<u64>,
        none_on: &[u64],
    ) -> DriverHarness {
        harness_config(specs, target_assets, format, fail_on, none_on, &[])
    }

    fn harness_config(
        specs: &[(u64, u32, u32, u32, u8)],
        target_assets: i32,
        format: i32,
        fail_on: Option<u64>,
        none_on: &[u64],
        missing: &[u64],
    ) -> DriverHarness {
        let bytes = specs
            .iter()
            .flat_map(|&(id, stored, offset, stride, fill)| {
                record(id, stored, offset, stride, fill)
            })
            .collect();
        let source = Arc::new(MemorySource::new(bytes));
        let events = Arc::new(Mutex::new(Vec::new()));
        let assets = Arc::new(Mutex::new(Vec::new()));
        let metadata_identity = Arc::new(());
        let wake = CollectionDecompressionWake::default();
        let driver = MountedResidentCollectionLoadDriver::new(
            source,
            MountedResidentCollectionLoadSpec {
                path: Arc::from("collection.xsf"),
                payload_offset: 0,
                request_id: 7,
                target_assets,
                format: CollectionStreamFormat(format),
            },
            TestMetadata {
                sizes: [1, 0, 0],
                identity: Arc::clone(&metadata_identity),
                missing: missing.iter().copied().collect(),
            },
            TestCollection {
                events: Arc::clone(&events),
                assets: Arc::clone(&assets),
            },
            TestBuilder {
                events: Arc::clone(&events),
                fail_on,
                none_on: none_on.iter().copied().collect(),
            },
            wake.clone(),
        )
        .unwrap();
        DriverHarness {
            driver,
            wake,
            events,
            assets,
            metadata_identity,
        }
    }

    fn small_specs(count: u64) -> Vec<(u64, u32, u32, u32, u8)> {
        (0..count)
            .map(|index| (index + 1, 1, 24, 32, u8::try_from(index + 1).unwrap()))
            .collect()
    }

    fn pump_until_pending(driver: &mut TestDriver, expected: usize) {
        for _ in 0..64 {
            if driver.pump().decompression().len() == expected {
                return;
            }
            driver.update_one_tick().unwrap();
        }
        panic!("driver did not queue {expected} decompression jobs");
    }

    #[test]
    fn format_dispatch_is_exact_for_three_special_values_and_default() {
        assert_eq!(
            CollectionAssetBuildMode::from_raw(CollectionStreamFormat(1)),
            CollectionAssetBuildMode::Compressed
        );
        assert_eq!(
            CollectionAssetBuildMode::from_raw(CollectionStreamFormat(2)),
            CollectionAssetBuildMode::CompressedArena
        );
        assert_eq!(
            CollectionAssetBuildMode::from_raw(CollectionStreamFormat(3)),
            CollectionAssetBuildMode::CompressedChunkArena
        );
        for raw in [i32::MIN, -1, 0, 4, i32::MAX] {
            assert_eq!(
                CollectionAssetBuildMode::from_raw(CollectionStreamFormat(raw)),
                CollectionAssetBuildMode::Plain
            );
        }
    }

    #[test]
    fn zero_target_is_rejected_before_producer_construction_or_io() {
        let source = Arc::new(MemorySource::new(Vec::new()));
        let source_observer = Arc::clone(&source);
        let events = Arc::new(Mutex::new(Vec::new()));
        let result = MountedResidentCollectionLoadDriver::new(
            source,
            MountedResidentCollectionLoadSpec {
                path: Arc::from("zero.xsf"),
                payload_offset: 0,
                request_id: 7,
                target_assets: 0,
                format: CollectionStreamFormat(0),
            },
            TestMetadata {
                sizes: [1, 0, 0],
                identity: Arc::new(()),
                missing: HashSet::new(),
            },
            TestCollection {
                events: Arc::clone(&events),
                assets: Arc::new(Mutex::new(Vec::new())),
            },
            TestBuilder {
                events,
                fail_on: None,
                none_on: HashSet::new(),
            },
            CollectionDecompressionWake::default(),
        );
        assert!(matches!(
            result,
            Err(error)
                if matches!(
                    &*error,
                    MountedResidentCollectionLoadDriverError::ZeroAssetRequest
                )
        ));
        assert!(lock_recover(&source_observer.calls).is_empty());
    }

    #[test]
    fn negative_target_uses_physical_path_and_ends_after_first_record() {
        let specs = small_specs(2);
        let mut harness = harness_with_target(&specs, -1, 0, None, &[]);
        let first = harness.driver.update_one_tick().unwrap();
        assert!(matches!(
            first.producer,
            MountedCollectionProducerPoll::Stage(ref stage)
                if stage.chunks_published == 1 && stage.finished
        ));
        assert_eq!(harness.driver.pump().parsed_assets(), 1);
        assert_eq!(harness.driver.pump().decompression().len(), 1);
        assert!(harness.wake.request());
        harness.driver.update_one_tick().unwrap();
        assert!(harness.driver.is_complete());
        assert_eq!(lock_recover(&harness.assets).as_slice(), [1]);
    }

    #[test]
    fn producer_error_latches_outer_arc_and_suppresses_later_work() {
        let mut harness = harness_with_target(&[], 1, 0, None, &[]);
        let first = harness.driver.update_one_tick().unwrap_err();
        assert!(matches!(
            &*first,
            MountedResidentCollectionLoadDriverError::Producer(source)
                if matches!(
                    &**source,
                    MountedCollectionProducerError::UnexpectedEndOfEntry { available: 0 }
                )
        ));
        assert!(harness.driver.pump().decompression().is_empty());
        assert!(lock_recover(&harness.events).is_empty());

        let second = harness.driver.update_one_tick().unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(harness.driver.pump().decompression().is_empty());
        assert!(lock_recover(&harness.events).is_empty());
    }

    #[test]
    fn pump_error_latches_outer_arc_and_retains_faulted_chunk() {
        let specs = small_specs(1);
        let mut harness = harness_config(&specs, 1, 0, None, &[], &[1]);
        let first = harness.driver.update_one_tick().unwrap_err();
        assert!(matches!(
            &*first,
            MountedResidentCollectionLoadDriverError::Pump(source)
                if matches!(&**source, CollectionLoadPumpError::MissingAssetInfo(1))
        ));
        assert!(harness.driver.pump().faulted_chunk().is_some());
        assert!(harness.driver.pump().decompression().is_empty());
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 0);

        let second = harness.driver.update_one_tick().unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(harness.driver.pump().faulted_chunk().is_some());
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 0);
    }

    #[test]
    fn submission_owns_metadata_and_non_releasing_bytes_until_woken() {
        let mut harness = harness(&small_specs(1), 3, None, &[]);
        let first = harness.driver.update_one_tick().unwrap();
        assert_eq!(first.decompression, DecompressionWakeProgress::default());
        assert!(lock_recover(&harness.events).is_empty());

        let queue = harness.driver.pump().decompression();
        let job = queue.front().unwrap();
        assert_eq!(job.asset_info.id, 1);
        assert!(Arc::ptr_eq(
            &job.asset_info.identity,
            &harness.metadata_identity
        ));
        assert_eq!(job.block.payload_bytes(24, 1), Some(&[1][..]));
        assert_eq!(Arc::strong_count(job.block.backing()), 2);
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 0);

        assert!(harness.wake.request());
        assert!(!harness.wake.request());
        let second = harness.driver.update_one_tick().unwrap();
        assert_eq!(
            second.decompression,
            DecompressionWakeProgress {
                snapshots: 1,
                source_slices: 1,
                jobs_completed: 1,
            }
        );
        assert!(harness.driver.pump().is_complete());
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 1);
        assert_eq!(lock_recover(&harness.assets).as_slice(), [1]);
    }

    #[test]
    fn wake_is_thread_safe_and_coalesces_concurrent_host_requests() {
        let wake = CollectionDecompressionWake::default();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let wake = wake.clone();
                thread::spawn(move || wake.request())
            })
            .collect();
        let newly_pending = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|&newly_pending| newly_pending)
            .count();
        assert_eq!(newly_pending, 1);
        assert!(wake.is_pending());
    }

    #[test]
    fn one_wake_drains_thirty_two_then_one_as_two_fifo_snapshots() {
        let specs = small_specs(33);
        let mut harness = harness(&specs, 0, None, &[]);
        pump_until_pending(&mut harness.driver, 33);
        assert!(harness.wake.request());
        let progress = harness.driver.update_one_tick().unwrap();
        assert_eq!(progress.decompression.snapshots, 2);
        assert_eq!(progress.decompression.source_slices, 2);
        assert_eq!(progress.decompression.jobs_completed, 33);
        assert_eq!(
            lock_recover(&harness.assets).as_slice(),
            (1..=33).collect::<Vec<_>>()
        );
    }

    fn source_budget_progress(stored_sizes: &[u32]) -> DecompressionWakeProgress {
        let specs: Vec<_> = stored_sizes
            .iter()
            .enumerate()
            .map(|(index, &stored)| {
                let stride = stored.checked_add(24).unwrap();
                (
                    u64::try_from(index).unwrap() + 1,
                    stored,
                    24,
                    stride,
                    u8::try_from(index + 1).unwrap(),
                )
            })
            .collect();
        let mut harness = harness(&specs, 0, None, &[]);
        pump_until_pending(&mut harness.driver, specs.len());
        assert!(harness.wake.request());
        harness.driver.update_one_tick().unwrap().decompression
    }

    #[test]
    fn source_budget_is_unsigned_pre_next_and_first_job_always_runs() {
        assert_eq!(
            source_budget_progress(&[DECOMPRESSION_SOURCE_BUDGET - 1, 1]).source_slices,
            1
        );
        assert_eq!(
            source_budget_progress(&[DECOMPRESSION_SOURCE_BUDGET, 1]).source_slices,
            2
        );
        assert_eq!(
            source_budget_progress(&[DECOMPRESSION_SOURCE_BUDGET + 1, 1]).source_slices,
            2
        );
        assert_eq!(
            source_budget_progress(&[DECOMPRESSION_SOURCE_BUDGET + 1]).jobs_completed,
            1
        );
    }

    #[test]
    fn builder_and_collection_observe_fifo_and_exact_modes() {
        for (raw, expected) in [
            (1, CollectionAssetBuildMode::Compressed),
            (2, CollectionAssetBuildMode::CompressedArena),
            (3, CollectionAssetBuildMode::CompressedChunkArena),
            (77, CollectionAssetBuildMode::Plain),
        ] {
            let mut harness = harness(&small_specs(3), raw, None, &[]);
            harness.driver.update_one_tick().unwrap();
            assert!(harness.wake.request());
            harness.driver.update_one_tick().unwrap();
            let events = lock_recover(&harness.events);
            for index in 0..3 {
                let pair = &events[index * 2..index * 2 + 2];
                let id = u64::try_from(index).unwrap() + 1;
                assert!(matches!(
                    pair,
                    [
                        Event::Build {
                            id: built,
                            info_id,
                            mode,
                            raw_format,
                            sequence,
                            ..
                        },
                        Event::Add(added)
                    ] if *built == id
                        && *info_id == id
                        && *mode == expected
                        && *raw_format == raw
                        && *sequence == u64::try_from(index).unwrap()
                        && *added == id
                ));
            }
        }
    }

    #[test]
    fn null_asset_completes_and_releases_without_loaded_accounting() {
        let mut harness = harness(&small_specs(1), 0, None, &[1]);
        harness.driver.update_one_tick().unwrap();
        assert!(harness.wake.request());
        harness.driver.update_one_tick().unwrap();
        assert!(lock_recover(&harness.assets).is_empty());
        assert_eq!(harness.driver.loaded_size(), 0);
        assert_eq!(harness.driver.pump().queue_tail(), 1);
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 1);
    }

    #[test]
    fn builder_failure_retains_failed_and_later_jobs_and_replays_arc() {
        let mut harness = harness(&small_specs(3), 0, Some(2), &[]);
        harness.driver.update_one_tick().unwrap();
        assert!(harness.wake.request());
        let first = harness.driver.update_one_tick().unwrap_err();
        assert!(matches!(
            &*first,
            MountedResidentCollectionLoadDriverError::Builder { asset_id: 2, .. }
        ));
        assert_eq!(harness.driver.pump().decompression().len(), 2);
        assert_eq!(
            harness
                .driver
                .pump()
                .decompression()
                .front()
                .unwrap()
                .asset_id,
            2
        );
        assert_eq!(harness.driver.pump().queue_tail(), 1);
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 1);
        assert_eq!(lock_recover(&harness.assets).as_slice(), [1]);
        assert!(
            !lock_recover(&harness.events)
                .iter()
                .any(|event| matches!(event, Event::Build { id: 3, .. }))
        );

        let event_count = lock_recover(&harness.events).len();
        let second = harness.driver.update_one_tick().unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(lock_recover(&harness.events).len(), event_count);
        assert_eq!(harness.driver.pump().decompression().len(), 2);
    }

    #[test]
    fn invalid_payload_retains_job_without_calling_builder_or_releasing_chunk() {
        let specs = [(1, 9, 24, 32, 0xaa)];
        let mut harness = harness(&specs, 0, None, &[]);
        harness.driver.update_one_tick().unwrap();
        assert!(harness.wake.request());
        let error = harness.driver.update_one_tick().unwrap_err();
        assert!(matches!(
            &*error,
            MountedResidentCollectionLoadDriverError::PayloadOutsideBlock {
                asset_id: 1,
                data_offset: 24,
                stored_size: 9,
                block_size: 32,
                ..
            }
        ));
        assert!(lock_recover(&harness.events).is_empty());
        assert_eq!(harness.driver.pump().decompression().len(), 1);
        assert_eq!(harness.driver.pump().queue_tail(), 0);
        assert_eq!(harness.driver.pump().stream().snapshot().released_chunks, 0);
    }

    #[test]
    fn resident_driver_trait_forwards_completion_and_accounting() {
        fn assert_trait<D, E>(driver: &mut D)
        where
            D: ResidentCollectionLoadDriver<E>,
            E: fmt::Debug,
        {
            driver.update().unwrap();
        }

        let mut harness = harness(&small_specs(1), 0, None, &[]);
        assert_trait(&mut harness.driver);
        assert!(harness.wake.request());
        assert_trait(&mut harness.driver);
        assert!(harness.driver.is_complete());
        assert_eq!(harness.driver.highest_stored_block(), 1);
        assert_eq!(harness.driver.loaded_size(), 1);
    }

    fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
