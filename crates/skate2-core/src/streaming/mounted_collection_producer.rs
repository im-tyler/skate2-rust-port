//! Mount-backed byte production for [`CollectionLoadPump`].
//!
//! This module deliberately stops at the native stream/parser boundary. It
//! stages logical file bytes, drives the recovered raw parser, and exposes
//! owned chunks through [`CollectionChunkStream`]. Native device scheduling,
//! cancellation, and failure collapse remain outside this safe host adapter.

use crate::collection_load_pump::{
    CollectionAssetMetadata, CollectionAssetRecord, CollectionChunkStream,
    CollectionDecompressionQueue, CollectionLoadPump, CollectionLoadPumpResult,
    CollectionParseDecision, CollectionStreamChunk, LoadedAssetCollection,
};
use skate2_formats::mount::{LogicalEntryRange, LogicalEntryStorage, MountError, MountSet};
use std::{
    collections::VecDeque,
    error::Error,
    fmt, io,
    ops::Range,
    sync::{Arc, Mutex, MutexGuard, Weak, mpsc},
};

/// Native `cAssetStreamer` file-read request size.
pub const COLLECTION_STREAM_READ_SIZE: u64 = 0x0004_0000;

/// Bytes available to the native request filename, excluding its forced NUL.
pub const COLLECTION_STREAM_PATH_LIMIT: usize = 255;

/// Injectable logical archive-entry range source.
///
/// Calls are synchronous, on the polling thread or the stream reader;
/// implementations must return exactly the requested logical range.
pub trait CollectionLogicalRangeSource: Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    /// Reads one exact logical range and reports the complete logical length.
    ///
    /// # Errors
    ///
    /// Returns the source's typed lookup, range, decoding, or I/O error.
    fn read_range(&self, path: &str, range: Range<u64>) -> Result<LogicalEntryRange, Self::Error>;
}

impl CollectionLogicalRangeSource for MountSet {
    type Error = MountError;

    fn read_range(&self, path: &str, range: Range<u64>) -> Result<LogicalEntryRange, Self::Error> {
        self.read_path_range(path, range)
    }
}

/// A source error whose chain carries a `WouldBlock` I/O error: a
/// resident-span miss. Not a failure — the read is not ready *yet*, and the
/// host's miss handler has already kicked the fetch that will make it ready.
/// File-backed sources never answer it, so native behavior is unchanged.
fn source_not_ready<E>(error: &E) -> bool
where
    E: Error + Send + Sync + 'static,
{
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(error) = current {
        if let Some(io_error) = error.downcast_ref::<io::Error>() {
            return io_error.kind() == io::ErrorKind::WouldBlock;
        }
        current = error.source();
    }
    false
}

/// Raw-parser surface required by the byte producer.
///
/// The producer asks whether the native one-record stride latch needs a new
/// header, then invokes the parser without holding its shared stream lock.
pub trait CollectionRawParser {
    type Error;

    fn needs_asset_header(&self) -> bool;

    /// Applies one native-shaped callback to the contiguous parse window.
    ///
    /// # Errors
    ///
    /// Returns the parser's first latched unsafe-domain error.
    fn parse_available(
        &mut self,
        available: u32,
        record: Option<&CollectionAssetRecord>,
    ) -> CollectionLoadPumpResult<CollectionParseDecision, Self::Error>;
}

impl<S, M, D, C> CollectionRawParser for CollectionLoadPump<S, M, D, C>
where
    S: CollectionChunkStream,
    M: CollectionAssetMetadata,
    M::Error: Error + Send + Sync + 'static,
    D: CollectionDecompressionQueue<S::Chunk, M::AssetInfo>,
    C: LoadedAssetCollection<D::Asset>,
{
    type Error = M::Error;

    fn needs_asset_header(&self) -> bool {
        self.next_asset()
    }

    fn parse_available(
        &mut self,
        available: u32,
        record: Option<&CollectionAssetRecord>,
    ) -> CollectionLoadPumpResult<CollectionParseDecision, Self::Error> {
        self.parse_raw(available, record)
    }
}

/// Sticky safe-host failure from the mounted producer boundary.
#[derive(Debug)]
pub enum MountedCollectionProducerError<E> {
    PathContainsNul,
    PathTooLong {
        length: usize,
    },
    EndedRequestId,
    Source(Arc<E>),
    PayloadOffsetOutsideEntry {
        offset: u64,
        logical_len: u64,
    },
    LogicalLengthChanged {
        expected: u64,
        observed: u64,
    },
    StorageChanged {
        expected: LogicalEntryStorage,
        observed: LogicalEntryStorage,
    },
    RangeLengthMismatch {
        expected: usize,
        observed: usize,
    },
    AvailableBytesTooLarge(usize),
    Parser(Arc<dyn Error + Send + Sync>),
    ParserBlockOutsideAvailable {
        block_size: u32,
        available: usize,
    },
    ParserChunkMissingAssetHeader {
        available: usize,
    },
    UnexpectedEndOfEntry {
        available: usize,
    },
    InternalInvariant(&'static str),
}

impl<E: fmt::Display> fmt::Display for MountedCollectionProducerError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathContainsNul => {
                formatter.write_str("collection path contains an interior NUL")
            }
            Self::PathTooLong { length } => write!(
                formatter,
                "collection path has {length} bytes; native QueueFile retains at most {COLLECTION_STREAM_PATH_LIMIT}"
            ),
            Self::EndedRequestId => {
                formatter.write_str("-1 is the ended collection-request sentinel")
            }
            Self::Source(source) => write!(formatter, "logical collection read failed: {source}"),
            Self::PayloadOffsetOutsideEntry {
                offset,
                logical_len,
            } => write!(
                formatter,
                "collection payload offset {offset:#x} is outside logical entry length {logical_len:#x}"
            ),
            Self::LogicalLengthChanged { expected, observed } => write!(
                formatter,
                "logical entry length changed from {expected:#x} to {observed:#x}"
            ),
            Self::StorageChanged { expected, observed } => write!(
                formatter,
                "logical entry storage changed from {expected:?} to {observed:?}"
            ),
            Self::RangeLengthMismatch { expected, observed } => write!(
                formatter,
                "logical range returned {observed} bytes instead of {expected}"
            ),
            Self::AvailableBytesTooLarge(size) => {
                write!(formatter, "parser availability {size} does not fit in u32")
            }
            Self::Parser(source) => write!(formatter, "collection parser failed: {source}"),
            Self::ParserBlockOutsideAvailable {
                block_size,
                available,
            } => write!(
                formatter,
                "parser emitted block {block_size:#x} with only {available:#x} bytes available"
            ),
            Self::ParserChunkMissingAssetHeader { available } => write!(
                formatter,
                "parser emitted a chunk with only {available} bytes of asset header"
            ),
            Self::UnexpectedEndOfEntry { available } => write!(
                formatter,
                "logical entry ended with {available} unconsumed collection bytes"
            ),
            Self::InternalInvariant(message) => {
                write!(formatter, "mounted collection invariant failed: {message}")
            }
        }
    }
}

impl<E: Error + 'static> Error for MountedCollectionProducerError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(source) => Some(source.as_ref()),
            Self::Parser(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// Result retaining a producer's first typed host failure by identity.
pub type MountedCollectionProducerResult<T, E> = Result<T, Arc<MountedCollectionProducerError<E>>>;

/// Arc-backed bytes retained from one parser-emitted native stride.
pub struct MountedCollectionChunk {
    backing: Arc<[u8]>,
    block_range: Range<usize>,
    source_range: Range<u64>,
    sequence: u64,
    stride: u32,
    origin: Weak<Mutex<SharedStreamState>>,
}

impl fmt::Debug for MountedCollectionChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountedCollectionChunk")
            .field("block_range", &self.block_range)
            .field("source_range", &self.source_range)
            .field("sequence", &self.sequence)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

impl MountedCollectionChunk {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.backing[self.block_range.clone()]
    }

    #[must_use]
    pub fn payload_bytes(&self, data_offset: u32, stored_size: u32) -> Option<&[u8]> {
        let start = usize::try_from(data_offset).ok()?;
        let size = usize::try_from(stored_size).ok()?;
        let end = start.checked_add(size)?;
        self.bytes().get(start..end)
    }

    #[must_use]
    pub fn source_range(&self) -> Range<u64> {
        self.source_range.clone()
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    #[must_use]
    pub fn backing(&self) -> &Arc<[u8]> {
        &self.backing
    }

    /// Retains an owned read-only view for deferred decompression without
    /// duplicating this stream handle's one-time release authority.
    #[must_use]
    pub fn owned_block_view(&self) -> MountedCollectionBlockView {
        MountedCollectionBlockView {
            backing: Arc::clone(&self.backing),
            block_range: self.block_range.clone(),
            source_range: self.source_range.clone(),
            sequence: self.sequence,
            stride: self.stride,
        }
    }
}

/// Cloneable read-only ownership of one parser-emitted block.
///
/// Unlike [`MountedCollectionChunk`], this type carries no stream origin and
/// therefore cannot release or otherwise mutate the producer/consumer
/// lifecycle. It is a high-confidence host ownership seam for deferred work.
#[derive(Clone)]
pub struct MountedCollectionBlockView {
    backing: Arc<[u8]>,
    block_range: Range<usize>,
    source_range: Range<u64>,
    sequence: u64,
    stride: u32,
}

impl fmt::Debug for MountedCollectionBlockView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountedCollectionBlockView")
            .field("block_range", &self.block_range)
            .field("source_range", &self.source_range)
            .field("sequence", &self.sequence)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

impl MountedCollectionBlockView {
    #[cfg(test)]
    pub(crate) fn from_test_bytes(bytes: Vec<u8>) -> Self {
        let length = bytes.len();
        Self {
            backing: Arc::from(bytes),
            block_range: 0..length,
            source_range: 0..u64::try_from(length).expect("test block length fits u64"),
            sequence: 0,
            stride: u32::try_from(length).expect("test block length fits u32"),
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.backing[self.block_range.clone()]
    }

    #[must_use]
    pub fn payload_bytes(&self, data_offset: u32, stored_size: u32) -> Option<&[u8]> {
        let start = usize::try_from(data_offset).ok()?;
        let size = usize::try_from(stored_size).ok()?;
        let end = start.checked_add(size)?;
        self.bytes().get(start..end)
    }

    #[must_use]
    pub fn source_range(&self) -> Range<u64> {
        self.source_range.clone()
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    #[must_use]
    pub fn backing(&self) -> &Arc<[u8]> {
        &self.backing
    }
}

/// Read-only diagnostics for the producer/consumer shared ownership boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MountedCollectionStreamSnapshot {
    pub queued_chunks: usize,
    pub retained_chunks: usize,
    pub retained_stride_bytes: u64,
    pub released_chunks: u64,
    pub accounted_stream_read_bytes: u64,
    pub producer_finished: bool,
    pub producer_failed: bool,
}

#[derive(Debug, Default)]
struct SharedStreamState {
    chunks: VecDeque<CollectionStreamChunk<MountedCollectionChunk>>,
    retained_chunks: usize,
    retained_stride_bytes: u64,
    released_chunks: u64,
    accounted_stream_read_bytes: u64,
    producer_finished: bool,
    producer_failed: bool,
    next_sequence: u64,
}

impl SharedStreamState {
    fn snapshot(&self) -> MountedCollectionStreamSnapshot {
        MountedCollectionStreamSnapshot {
            queued_chunks: self.chunks.len(),
            retained_chunks: self.retained_chunks,
            retained_stride_bytes: self.retained_stride_bytes,
            released_chunks: self.released_chunks,
            accounted_stream_read_bytes: self.accounted_stream_read_bytes,
            producer_finished: self.producer_finished,
            producer_failed: self.producer_failed,
        }
    }
}

/// Consumer half passed into [`CollectionLoadPump`].
///
/// Chunks are source ordered and uniquely owned. The backing allocation stays
/// alive across decompression until `release_chunk` consumes the handle.
pub struct MountedCollectionChunkStream {
    shared: Arc<Mutex<SharedStreamState>>,
}

impl fmt::Debug for MountedCollectionChunkStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountedCollectionChunkStream")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl MountedCollectionChunkStream {
    #[must_use]
    pub fn snapshot(&self) -> MountedCollectionStreamSnapshot {
        lock_recover(&self.shared).snapshot()
    }
}

impl CollectionChunkStream for MountedCollectionChunkStream {
    type Chunk = MountedCollectionChunk;

    fn is_end_of_stream(&mut self) -> bool {
        let shared = lock_recover(&self.shared);
        shared.producer_finished && shared.chunks.is_empty()
    }

    fn get_chunk(&mut self) -> Option<CollectionStreamChunk<Self::Chunk>> {
        lock_recover(&self.shared).chunks.pop_front()
    }

    fn release_chunk(&mut self, chunk: Self::Chunk) {
        if let Some(origin) = chunk.origin.upgrade() {
            let mut shared = lock_recover(&origin);
            debug_assert!(shared.retained_chunks > 0);
            debug_assert!(shared.retained_stride_bytes >= u64::from(chunk.stride));
            shared.retained_chunks -= 1;
            shared.retained_stride_bytes -= u64::from(chunk.stride);
            shared.released_chunks = shared.released_chunks.wrapping_add(1);
        }
    }

    fn account_stream_read_bytes(&mut self, stride: u32) {
        let mut shared = lock_recover(&self.shared);
        shared.accounted_stream_read_bytes = shared
            .accounted_stream_read_bytes
            .wrapping_add(u64::from(stride));
    }
}

/// Work performed by one logical-range producer poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountedCollectionStageProgress {
    pub range: Range<u64>,
    pub storage: LogicalEntryStorage,
    pub bytes_read: usize,
    pub parser_callbacks: u32,
    pub chunks_published: u32,
    pub buffered_bytes: usize,
    pub finished: bool,
}

/// Result of one producer poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountedCollectionProducerPoll {
    Stage(MountedCollectionStageProgress),
    /// The stream reader has not finished the next stage.
    Reading,
    Finished,
}

/// One stage the stream reader read: the entry's length probe with the first, then the
/// stage's range and bytes (none when the probe leaves nothing past the cursor).
struct BackgroundStage {
    probe: Option<LogicalEntryRange>,
    range: Range<u64>,
    read: Option<LogicalEntryRange>,
}

/// The stream reader's answer for the stage it reads.
type StageReading<E> = Mutex<mpsc::Receiver<Result<BackgroundStage, E>>>;

/// The stream reader's read of one stage at `cursor`: the length probe first while the length
/// is unknown, then the stage, none when the entry ends at the cursor.
fn read_background_stage<R: CollectionLogicalRangeSource>(
    source: &R,
    path: &str,
    cursor: u64,
    logical_len: Option<u64>,
) -> Result<BackgroundStage, R::Error> {
    let probe = match logical_len {
        Some(_) => None,
        None => Some(source.read_range(path, 0..0)?),
    };
    let length = logical_len.or_else(|| probe.as_ref().map(|probe| probe.logical_len)).unwrap_or_default();
    let range = cursor..length.min(cursor.saturating_add(COLLECTION_STREAM_READ_SIZE));
    let read = if cursor < length { Some(source.read_range(path, range.clone())?) } else { None };
    Ok(BackgroundStage { probe, range, read })
}

/// Logical-byte producer paired with a unique chunk consumer.
///
/// Exact native anchors are `Stream::restartstream` at `0x82bbed50`,
/// `Stream::parsechunks` at `0x82bbc388`, and the collection parser at
/// `0x82d546b0`. The separate length probe, the reads (on the stream reader, or
/// on the polling thread in a traced run), owned chunk copies, and typed
/// terminal errors are safe PC policy.
pub struct MountedCollectionProducer<R>
where
    R: CollectionLogicalRangeSource,
{
    source: Arc<R>,
    path: Arc<str>,
    request_id: i32,
    payload_offset: u64,
    aligned_offset: u64,
    prefix_to_discard: usize,
    logical_len: Option<u64>,
    storage: Option<LogicalEntryStorage>,
    read_cursor: u64,
    buffer_source_start: u64,
    buffer: Vec<u8>,
    shared: Arc<Mutex<SharedStreamState>>,
    finished: bool,
    terminal_error: Option<Arc<MountedCollectionProducerError<R::Error>>>,
    /// Stages read on the stream reader ([`crate::stream_reads`]) rather than the polling
    /// thread: every run but a traced one ([`crate::diagnostics::traced_run`]) and the tests.
    background: bool,
    /// The stage the stream reader is reading.
    reading: Option<StageReading<R::Error>>,
}

impl<R> MountedCollectionProducer<R>
where
    R: CollectionLogicalRangeSource,
{
    /// Creates paired producer and consumer ownership without performing I/O.
    ///
    /// The path byte limit reproduces `QueueFile`'s 255-byte retained C-string
    /// payload. Interior NULs are rejected rather than silently truncating.
    ///
    /// # Errors
    ///
    /// Rejects an interior NUL, a path longer than 255 bytes, or the ended
    /// request sentinel before constructing either half.
    pub fn new(
        source: Arc<R>,
        path: impl Into<Arc<str>>,
        payload_offset: u64,
        request_id: i32,
    ) -> MountedCollectionProducerResult<(Self, MountedCollectionChunkStream), R::Error> {
        let path = path.into();
        if path.as_bytes().contains(&0) {
            return Err(Arc::new(MountedCollectionProducerError::PathContainsNul));
        }
        if path.len() > COLLECTION_STREAM_PATH_LIMIT {
            return Err(Arc::new(MountedCollectionProducerError::PathTooLong {
                length: path.len(),
            }));
        }
        if request_id == -1 {
            return Err(Arc::new(MountedCollectionProducerError::EndedRequestId));
        }

        let aligned_offset = payload_offset & !3;
        let prefix_to_discard =
            usize::from(u8::try_from(payload_offset - aligned_offset).unwrap_or_default());
        let shared = Arc::new(Mutex::new(SharedStreamState::default()));
        let consumer = MountedCollectionChunkStream {
            shared: Arc::clone(&shared),
        };
        Ok((
            Self {
                source,
                path,
                request_id,
                payload_offset,
                aligned_offset,
                prefix_to_discard,
                logical_len: None,
                storage: None,
                read_cursor: aligned_offset,
                buffer_source_start: payload_offset,
                buffer: Vec::new(),
                shared,
                finished: false,
                terminal_error: None,
                background: !cfg!(test) && !crate::diagnostics::traced_run(),
                reading: None,
            },
            consumer,
        ))
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn request_id(&self) -> i32 {
        self.request_id
    }

    #[must_use]
    pub const fn payload_offset(&self) -> u64 {
        self.payload_offset
    }

    #[must_use]
    pub const fn aligned_offset(&self) -> u64 {
        self.aligned_offset
    }

    #[must_use]
    pub const fn logical_len(&self) -> Option<u64> {
        self.logical_len
    }

    #[must_use]
    pub fn terminal_error(&self) -> Option<&Arc<MountedCollectionProducerError<R::Error>>> {
        self.terminal_error.as_ref()
    }

    /// Reads one native-sized logical stage and drives all parser decisions
    /// possible from the newly accumulated bytes.
    ///
    /// Source reads and parser callbacks occur with no shared stream lock held.
    /// A first source, parser, range-contract, or EOF failure is retained and
    /// returned unchanged by every later poll. A source read that answers
    /// `WouldBlock` (a resident-span miss) is none of these: the poll reports
    /// [`MountedCollectionProducerPoll::Reading`] without latching, and the
    /// next poll re-attempts the same range.
    ///
    /// # Errors
    ///
    /// Returns and permanently latches the first source, parser, source-range,
    /// offset, or premature-EOF error.
    pub fn poll_parser<P>(
        &mut self,
        parser: &mut P,
    ) -> MountedCollectionProducerResult<MountedCollectionProducerPoll, R::Error>
    where
        P: CollectionRawParser,
        P::Error: Error + Send + Sync + 'static,
    {
        if let Some(error) = &self.terminal_error {
            return Err(Arc::clone(error));
        }
        if self.finished {
            return Ok(MountedCollectionProducerPoll::Finished);
        }
        let background = if self.background {
            let Some(stage) = self.take_background_stage()? else {
                return Ok(MountedCollectionProducerPoll::Reading);
            };
            Some(stage)
        } else {
            if self.logical_len.is_none() && !self.initialize_entry()? {
                return Ok(MountedCollectionProducerPoll::Reading);
            }
            None
        };

        let Some(logical_len) = self.logical_len else {
            return Err(
                self.latch(MountedCollectionProducerError::InternalInvariant(
                    "length probe completed without retaining logical length",
                )),
            );
        };
        if self.read_cursor >= logical_len {
            let available = self.buffer.len();
            return Err(
                self.latch(MountedCollectionProducerError::UnexpectedEndOfEntry { available })
            );
        }

        let (range, read) = match background {
            Some(BackgroundStage { range, read: Some(read), .. }) => {
                self.validate_read(&range, &read)?;
                (range, read)
            }
            Some(BackgroundStage { read: None, .. }) => {
                return Err(self.latch(MountedCollectionProducerError::InternalInvariant(
                    "the stream reader read no stage before the entry's end",
                )));
            }
            None => match self.read_stage(logical_len)? {
                Some(read) => read,
                None => return Ok(MountedCollectionProducerPoll::Reading),
            },
        };
        let bytes_read = read.bytes.len();
        let storage = read.storage;
        self.append_stage(read.bytes, range.end);
        let (parser_callbacks, chunks_published) = self.drive_parser(parser)?;

        if !self.finished && self.read_cursor == logical_len {
            return Err(
                self.latch(MountedCollectionProducerError::UnexpectedEndOfEntry {
                    available: self.buffer.len(),
                }),
            );
        }
        if self.background && !self.finished {
            self.queue_background_stage(Some(logical_len));
        }

        Ok(MountedCollectionProducerPoll::Stage(
            MountedCollectionStageProgress {
                range,
                storage,
                bytes_read,
                parser_callbacks,
                chunks_published,
                buffered_bytes: self.buffer.len(),
                finished: self.finished,
            },
        ))
    }

    /// The stage the stream reader finished, its length probe taken in as
    /// [`Self::initialize_entry`] takes its own; none while it reads. The first poll queues
    /// the probe and the first stage.
    fn take_background_stage(
        &mut self,
    ) -> MountedCollectionProducerResult<Option<BackgroundStage>, R::Error> {
        let Some(reading) = self.reading.as_mut() else {
            self.queue_background_stage(None);
            return Ok(None);
        };
        let received = reading
            .get_mut()
            .map_or(Err(mpsc::TryRecvError::Disconnected), |receiver| receiver.try_recv());
        let stage = match received {
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.reading = None;
                return Err(self.latch(MountedCollectionProducerError::InternalInvariant(
                    "the stream reader dropped a stage",
                )));
            }
            Ok(Err(error)) => {
                self.reading = None;
                if source_not_ready(&error) {
                    // The stream reader's stage answered "not ready": re-queue
                    // the same stage (its range is recomputed from the
                    // cursor) and report Reading — nothing latched, nothing
                    // advanced.
                    self.queue_background_stage(self.logical_len);
                    return Ok(None);
                }
                return Err(self.latch(MountedCollectionProducerError::Source(Arc::new(error))));
            }
            Ok(Ok(stage)) => stage,
        };
        self.reading = None;
        if let Some(probe) = &stage.probe {
            self.take_probe(probe)?;
        }
        Ok(Some(stage))
    }

    /// Queues [`Self::read_stage`]'s next range on the stream reader, the first behind
    /// [`Self::initialize_entry`]'s length probe.
    fn queue_background_stage(&mut self, logical_len: Option<u64>) {
        let (source, path, cursor) = (Arc::clone(&self.source), Arc::clone(&self.path), self.read_cursor);
        let (sender, receiver) = mpsc::channel();
        crate::stream_reads::queue(move || {
            let _ = sender.send(read_background_stage(&*source, &path, cursor, logical_len));
        });
        self.reading = Some(Mutex::new(receiver));
    }

    fn read_stage(
        &mut self,
        logical_len: u64,
    ) -> MountedCollectionProducerResult<Option<(Range<u64>, LogicalEntryRange)>, R::Error>
    {
        let remaining = logical_len - self.read_cursor;
        let amount = remaining.min(COLLECTION_STREAM_READ_SIZE);
        let end = self.read_cursor + amount;
        let range = self.read_cursor..end;
        let read = match self.source.read_range(&self.path, range.clone()) {
            Ok(read) => read,
            // Not ready: nothing latched, nothing advanced; the next poll
            // recomputes this same range from the cursor.
            Err(error) if source_not_ready(&error) => return Ok(None),
            Err(error) => {
                return Err(self.latch(MountedCollectionProducerError::Source(Arc::new(error))));
            }
        };
        self.validate_read(&range, &read)?;
        Ok(Some((range, read)))
    }

    fn append_stage(&mut self, mut bytes: Vec<u8>, end: u64) {
        if self.read_cursor == self.aligned_offset && self.prefix_to_discard != 0 {
            bytes.drain(..self.prefix_to_discard);
        }
        self.buffer.extend_from_slice(&bytes);
        self.read_cursor = end;
    }

    fn drive_parser<P>(
        &mut self,
        parser: &mut P,
    ) -> MountedCollectionProducerResult<(u32, u32), R::Error>
    where
        P: CollectionRawParser,
        P::Error: Error + Send + Sync + 'static,
    {
        let mut parser_callbacks = 0_u32;
        let mut chunks_published = 0_u32;
        loop {
            let Ok(available) = u32::try_from(self.buffer.len()) else {
                return Err(
                    self.latch(MountedCollectionProducerError::AvailableBytesTooLarge(
                        self.buffer.len(),
                    )),
                );
            };
            let record = if available > 24 && parser.needs_asset_header() {
                parse_asset_record(&self.buffer)
            } else {
                None
            };
            parser_callbacks = parser_callbacks.wrapping_add(1);
            let decision = match parser.parse_available(available, record.as_ref()) {
                Ok(decision) => decision,
                Err(error) => {
                    let error: Arc<dyn Error + Send + Sync> = error;
                    return Err(self.latch(MountedCollectionProducerError::Parser(error)));
                }
            };
            let (block_size, end_request) = match decision {
                CollectionParseDecision::MoreDataNeeded => break,
                CollectionParseDecision::Chunk { block_size } => (block_size, false),
                CollectionParseDecision::ChunkEndRequest { block_size } => (block_size, true),
            };
            let Ok(block_len) = usize::try_from(block_size) else {
                return Err(
                    self.latch(MountedCollectionProducerError::InternalInvariant(
                        "parser block size did not fit usize",
                    )),
                );
            };
            if block_size == 0 || block_len > self.buffer.len() {
                return Err(self.latch(
                    MountedCollectionProducerError::ParserBlockOutsideAvailable {
                        block_size,
                        available: self.buffer.len(),
                    },
                ));
            }

            let Some(record) = parse_asset_record(&self.buffer) else {
                return Err(self.latch(
                    MountedCollectionProducerError::ParserChunkMissingAssetHeader {
                        available: self.buffer.len(),
                    },
                ));
            };
            let block: Arc<[u8]> = Arc::from(self.buffer[..block_len].to_vec());
            let Some(source_end) = self.buffer_source_start.checked_add(u64::from(block_size))
            else {
                return Err(
                    self.latch(MountedCollectionProducerError::InternalInvariant(
                        "published source range overflowed u64",
                    )),
                );
            };
            let mut shared = lock_recover(&self.shared);
            let sequence = shared.next_sequence;
            shared.next_sequence = shared.next_sequence.wrapping_add(1);
            shared.retained_chunks = shared.retained_chunks.wrapping_add(1);
            shared.retained_stride_bytes = shared
                .retained_stride_bytes
                .wrapping_add(u64::from(block_size));
            shared.chunks.push_back(CollectionStreamChunk {
                request_id: self.request_id,
                record,
                handle: MountedCollectionChunk {
                    backing: block,
                    block_range: 0..block_len,
                    source_range: self.buffer_source_start..source_end,
                    sequence,
                    stride: block_size,
                    origin: Arc::downgrade(&self.shared),
                },
            });
            if end_request {
                shared.producer_finished = true;
            }
            drop(shared);

            self.buffer.drain(..block_len);
            self.buffer_source_start = source_end;
            chunks_published = chunks_published.wrapping_add(1);
            if end_request {
                self.buffer.clear();
                self.finished = true;
                break;
            }
        }
        Ok((parser_callbacks, chunks_published))
    }

    fn initialize_entry(&mut self) -> MountedCollectionProducerResult<bool, R::Error> {
        let read = match self.source.read_range(&self.path, 0..0) {
            Ok(read) => read,
            // Not ready: nothing latched; the next poll probes again.
            Err(error) if source_not_ready(&error) => return Ok(false),
            Err(error) => {
                return Err(self.latch(MountedCollectionProducerError::Source(Arc::new(error))));
            }
        };
        self.take_probe(&read)?;
        Ok(true)
    }

    /// The length probe: no bytes, and the payload inside the entry.
    fn take_probe(&mut self, read: &LogicalEntryRange) -> MountedCollectionProducerResult<(), R::Error> {
        if !read.bytes.is_empty() {
            return Err(
                self.latch(MountedCollectionProducerError::RangeLengthMismatch {
                    expected: 0,
                    observed: read.bytes.len(),
                }),
            );
        }
        if self.payload_offset > read.logical_len {
            return Err(
                self.latch(MountedCollectionProducerError::PayloadOffsetOutsideEntry {
                    offset: self.payload_offset,
                    logical_len: read.logical_len,
                }),
            );
        }
        self.logical_len = Some(read.logical_len);
        self.storage = Some(read.storage);
        Ok(())
    }

    fn validate_read(
        &mut self,
        range: &Range<u64>,
        read: &LogicalEntryRange,
    ) -> MountedCollectionProducerResult<(), R::Error> {
        let Some(expected_len) = self.logical_len else {
            return Err(
                self.latch(MountedCollectionProducerError::InternalInvariant(
                    "stage validation ran before the length probe",
                )),
            );
        };
        if read.logical_len != expected_len {
            return Err(
                self.latch(MountedCollectionProducerError::LogicalLengthChanged {
                    expected: expected_len,
                    observed: read.logical_len,
                }),
            );
        }
        let Some(expected_storage) = self.storage else {
            return Err(
                self.latch(MountedCollectionProducerError::InternalInvariant(
                    "stage validation ran before retaining storage kind",
                )),
            );
        };
        if read.storage != expected_storage {
            return Err(self.latch(MountedCollectionProducerError::StorageChanged {
                expected: expected_storage,
                observed: read.storage,
            }));
        }
        let Ok(expected) = usize::try_from(range.end - range.start) else {
            return Err(
                self.latch(MountedCollectionProducerError::InternalInvariant(
                    "one native read stage did not fit usize",
                )),
            );
        };
        if read.bytes.len() != expected {
            return Err(
                self.latch(MountedCollectionProducerError::RangeLengthMismatch {
                    expected,
                    observed: read.bytes.len(),
                }),
            );
        }
        Ok(())
    }

    fn latch(
        &mut self,
        error: MountedCollectionProducerError<R::Error>,
    ) -> Arc<MountedCollectionProducerError<R::Error>> {
        if let Some(first) = &self.terminal_error {
            return Arc::clone(first);
        }
        let error = Arc::new(error);
        self.terminal_error = Some(Arc::clone(&error));
        lock_recover(&self.shared).producer_failed = true;
        error
    }
}

fn parse_asset_record(bytes: &[u8]) -> Option<CollectionAssetRecord> {
    Some(CollectionAssetRecord {
        asset_id: u64::from_be_bytes(bytes.get(0..8)?.try_into().ok()?),
        stored_size: u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?),
        data_offset: u32::from_be_bytes(bytes.get(12..16)?.try_into().ok()?),
        stride: u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
    })
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection_load_pump::{CollectionDecompressionSubmission, CollectionStreamFormat};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    #[derive(Debug)]
    struct MemorySource {
        base: u64,
        bytes: Arc<[u8]>,
        logical_len: u64,
        storage: LogicalEntryStorage,
        calls: Mutex<Vec<Range<u64>>>,
        fail_at_call: Option<usize>,
        /// Calls whose index falls in this window answer `WouldBlock`: a
        /// resident-span miss, not a failure.
        not_ready_calls: Option<Range<usize>>,
        wrong_len_at_call: Option<usize>,
        changed_storage_at_call: Option<usize>,
        changed_logical_len_at_call: Option<usize>,
    }

    impl MemorySource {
        fn new(base: u64, bytes: Vec<u8>) -> Self {
            Self {
                base,
                logical_len: base + u64::try_from(bytes.len()).unwrap(),
                bytes: Arc::from(bytes),
                storage: LogicalEntryStorage::Raw,
                calls: Mutex::new(Vec::new()),
                fail_at_call: None,
                not_ready_calls: None,
                wrong_len_at_call: None,
                changed_storage_at_call: None,
                changed_logical_len_at_call: None,
            }
        }

        fn calls(&self) -> Vec<Range<u64>> {
            lock_recover(&self.calls).clone()
        }
    }

    impl CollectionLogicalRangeSource for MemorySource {
        type Error = io::Error;

        fn read_range(
            &self,
            _path: &str,
            range: Range<u64>,
        ) -> Result<LogicalEntryRange, Self::Error> {
            let mut calls = lock_recover(&self.calls);
            let call = calls.len();
            calls.push(range.clone());
            drop(calls);
            if self.fail_at_call == Some(call) {
                return Err(io::Error::other("injected read failure"));
            }
            if let Some(blocked) = &self.not_ready_calls {
                if blocked.contains(&call) {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
            }
            let mut bytes = if range.is_empty() {
                Vec::new()
            } else {
                let start = usize::try_from(range.start - self.base).unwrap();
                let end = usize::try_from(range.end - self.base).unwrap();
                self.bytes[start..end].to_vec()
            };
            if self.wrong_len_at_call == Some(call) {
                bytes.pop();
            }
            let storage = if self.changed_storage_at_call == Some(call) {
                LogicalEntryStorage::RefPack
            } else {
                self.storage
            };
            let logical_len = if self.changed_logical_len_at_call == Some(call) {
                self.logical_len + 1
            } else {
                self.logical_len
            };
            Ok(LogicalEntryRange {
                bytes,
                logical_len,
                storage,
            })
        }
    }

    #[derive(Default)]
    struct EmptyMetadata;

    impl CollectionAssetMetadata for EmptyMetadata {
        type AssetInfo = ();
        type Error = std::convert::Infallible;

        fn find_asset_info(
            &mut self,
            _asset_id: u64,
        ) -> Result<Option<Self::AssetInfo>, Self::Error> {
            Ok(Some(()))
        }

        fn first_three_resource_sizes(&mut self, _info: &Self::AssetInfo) -> [u32; 3] {
            [0; 3]
        }
    }

    #[derive(Default)]
    struct NoDecompression;

    impl CollectionDecompressionQueue<MountedCollectionChunk, ()> for NoDecompression {
        type Asset = ();

        fn submit(
            &mut self,
            _submission: CollectionDecompressionSubmission<'_, MountedCollectionChunk, ()>,
        ) {
            panic!("zero destination sizes must not submit decompression");
        }
    }

    #[derive(Default)]
    struct NoAssets;

    impl LoadedAssetCollection<()> for NoAssets {
        fn add_asset(&mut self, _asset: ()) {
            panic!("zero destination sizes must not add assets");
        }
    }

    fn record(id: u64, stride: usize, fill: u8) -> Vec<u8> {
        assert!(stride > 24);
        let mut bytes = vec![fill; stride];
        bytes[0..8].copy_from_slice(&id.to_be_bytes());
        bytes[8..12].copy_from_slice(&(u32::try_from(stride - 24).unwrap()).to_be_bytes());
        bytes[12..16].copy_from_slice(&24_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&(u32::try_from(stride).unwrap()).to_be_bytes());
        bytes[20..24].fill(0);
        bytes
    }

    fn pump(
        stream: MountedCollectionChunkStream,
        request_id: i32,
        target: i32,
    ) -> CollectionLoadPump<MountedCollectionChunkStream, EmptyMetadata, NoDecompression, NoAssets>
    {
        CollectionLoadPump::new(
            stream,
            EmptyMetadata,
            NoDecompression,
            NoAssets,
            request_id,
            target,
            CollectionStreamFormat(0),
        )
    }

    #[test]
    fn path_limit_nul_and_ended_request_are_rejected_before_io() {
        let source = Arc::new(MemorySource::new(0, vec![0; 32]));
        assert!(MountedCollectionProducer::new(Arc::clone(&source), "x".repeat(255), 0, 1).is_ok());
        assert!(matches!(
            MountedCollectionProducer::new(Arc::clone(&source), "x".repeat(256), 0, 1),
            Err(error) if matches!(&*error, MountedCollectionProducerError::PathTooLong { length: 256 })
        ));
        assert!(matches!(
            MountedCollectionProducer::new(Arc::clone(&source), "bad\0path", 0, 1),
            Err(error) if matches!(&*error, MountedCollectionProducerError::PathContainsNul)
        ));
        assert!(matches!(
            MountedCollectionProducer::new(Arc::clone(&source), "file.xsf", 0, -1),
            Err(error) if matches!(&*error, MountedCollectionProducerError::EndedRequestId)
        ));
        assert!(source.calls().is_empty());
    }

    #[test]
    fn full_u64_offset_aligns_down_four_and_discards_only_prefix() {
        let offset = 0x0000_0001_0000_0003_u64;
        let aligned = offset & !3;
        let mut bytes = vec![0xaa, 0xbb, 0xcc];
        bytes.extend(record(0x1122_3344_5566_7788, 128, 0x5a));
        let source = Arc::new(MemorySource::new(aligned, bytes));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "world\\cell.xsf", offset, 7)
                .unwrap();
        let mut parser = pump(
            MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
            7,
            1,
        );
        let progress = producer.poll_parser(&mut parser).unwrap();
        assert!(matches!(progress, MountedCollectionProducerPoll::Stage(stage) if stage.finished));
        assert_eq!(producer.payload_offset(), offset);
        assert_eq!(producer.aligned_offset(), aligned);
        assert_eq!(source.calls(), vec![0..0, aligned..aligned + 131]);

        let chunk = stream.get_chunk().unwrap();
        assert_eq!(chunk.record.asset_id, 0x1122_3344_5566_7788);
        assert_eq!(chunk.handle.source_range(), offset..offset + 128);
        assert_eq!(chunk.handle.bytes().len(), 128);
        assert_eq!(chunk.handle.bytes()[24], 0x5a);
        stream.release_chunk(chunk.handle);
    }

    #[test]
    fn one_stage_emits_multiple_source_ordered_chunks_and_stops_on_raw_two() {
        let mut bytes = record(1, 128, 0x11);
        bytes.extend(record(2, 256, 0x22));
        bytes.extend(vec![0xee; 4096]);
        let source = Arc::new(MemorySource::new(0, bytes));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(source, "cells.xsf", 0, 0x101).unwrap();
        let parser_stream = MountedCollectionChunkStream {
            shared: Arc::clone(&stream.shared),
        };
        let mut parser = pump(parser_stream, 0x101, 2);

        let MountedCollectionProducerPoll::Stage(stage) =
            producer.poll_parser(&mut parser).unwrap()
        else {
            panic!("first poll must read a stage");
        };
        assert_eq!(stage.parser_callbacks, 2);
        assert_eq!(stage.chunks_published, 2);
        assert!(stage.finished);
        assert_eq!(source_ids(&mut stream), vec![1, 2]);
        assert!(matches!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Finished
        ));
    }

    #[test]
    fn stride_latch_accumulates_across_native_sized_stages() {
        let stride = usize::try_from(COLLECTION_STREAM_READ_SIZE * 2).unwrap();
        let source = Arc::new(MemorySource::new(0, record(9, stride, 0x44)));
        let (mut producer, stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "large.xsf", 0, 3).unwrap();
        let mut parser = pump(stream, 3, 1);

        let MountedCollectionProducerPoll::Stage(first) =
            producer.poll_parser(&mut parser).unwrap()
        else {
            panic!("first poll must read");
        };
        assert_eq!(
            first.buffered_bytes,
            usize::try_from(COLLECTION_STREAM_READ_SIZE).unwrap()
        );
        assert_eq!(first.chunks_published, 0);
        assert!(!parser.needs_asset_header());
        assert_eq!(parser.pending_stride(), u32::try_from(stride).unwrap());

        let MountedCollectionProducerPoll::Stage(second) =
            producer.poll_parser(&mut parser).unwrap()
        else {
            panic!("second poll must read");
        };
        assert_eq!(second.chunks_published, 1);
        assert!(second.finished);
        assert_eq!(source.calls(), vec![0..0, 0..0x40000, 0x40000..0x80000]);
    }

    #[test]
    fn maximum_stride_requires_exactly_ten_native_sized_stages() {
        let stride =
            usize::try_from(crate::collection_load_pump::COLLECTION_LOAD_MAX_STRIDE).unwrap();
        let source = Arc::new(MemorySource::new(0, record(0xfeed, stride, 0x3c)));
        let (mut producer, stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "maximum.xsf", 0, 6).unwrap();
        let mut parser = pump(stream, 6, 1);

        for index in 0..10 {
            let MountedCollectionProducerPoll::Stage(stage) =
                producer.poll_parser(&mut parser).unwrap()
            else {
                panic!("each input stage must be reported");
            };
            assert_eq!(stage.chunks_published, u32::from(index == 9));
            assert_eq!(stage.finished, index == 9);
        }
        assert_eq!(source.calls().len(), 11);
        assert_eq!(source.calls()[1], 0..0x40000);
        assert_eq!(source.calls()[10], 0x0024_0000..0x0028_0000);
    }

    #[test]
    fn producer_preserves_strict_more_than_twenty_four_callback_gate() {
        struct RecordingParser {
            calls: Vec<(u32, bool)>,
        }

        impl CollectionRawParser for RecordingParser {
            type Error = std::convert::Infallible;

            fn needs_asset_header(&self) -> bool {
                true
            }

            fn parse_available(
                &mut self,
                available: u32,
                record: Option<&CollectionAssetRecord>,
            ) -> CollectionLoadPumpResult<CollectionParseDecision, Self::Error> {
                self.calls.push((available, record.is_some()));
                Ok(CollectionParseDecision::MoreDataNeeded)
            }
        }

        for (length, expected_record) in [(24, false), (25, true)] {
            let source = Arc::new(MemorySource::new(0, vec![0; length]));
            let (mut producer, _stream) =
                MountedCollectionProducer::new(source, "gate.xsf", 0, 1).unwrap();
            let mut parser = RecordingParser { calls: Vec::new() };
            assert!(producer.poll_parser(&mut parser).is_err());
            assert_eq!(
                parser.calls,
                vec![(u32::try_from(length).unwrap(), expected_record)]
            );
        }
    }

    #[test]
    fn invalid_parser_chunk_shapes_become_typed_sticky_errors() {
        struct FixedParser(CollectionParseDecision);

        impl CollectionRawParser for FixedParser {
            type Error = std::convert::Infallible;

            fn needs_asset_header(&self) -> bool {
                false
            }

            fn parse_available(
                &mut self,
                _available: u32,
                _record: Option<&CollectionAssetRecord>,
            ) -> CollectionLoadPumpResult<CollectionParseDecision, Self::Error> {
                Ok(self.0)
            }
        }

        let source = Arc::new(MemorySource::new(0, vec![0; 32]));
        let (mut producer, _stream) =
            MountedCollectionProducer::new(source, "oversized.xsf", 0, 1).unwrap();
        let error = producer
            .poll_parser(&mut FixedParser(CollectionParseDecision::Chunk {
                block_size: 33,
            }))
            .unwrap_err();
        assert!(matches!(
            &*error,
            MountedCollectionProducerError::ParserBlockOutsideAvailable {
                block_size: 33,
                available: 32
            }
        ));

        let source = Arc::new(MemorySource::new(0, vec![0; 1]));
        let (mut producer, _stream) =
            MountedCollectionProducer::new(source, "headerless.xsf", 0, 1).unwrap();
        let first = producer
            .poll_parser(&mut FixedParser(CollectionParseDecision::Chunk {
                block_size: 1,
            }))
            .unwrap_err();
        let second = producer
            .poll_parser(&mut FixedParser(CollectionParseDecision::Chunk {
                block_size: 1,
            }))
            .unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(
            &*first,
            MountedCollectionProducerError::ParserChunkMissingAssetHeader { available: 1 }
        ));
    }

    #[test]
    fn eos_becomes_visible_only_after_final_chunk_is_dequeued() {
        let source = Arc::new(MemorySource::new(0, record(4, 128, 0x33)));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(source, "one.xsf", 0, 5).unwrap();
        let mut parser = pump(
            MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
            5,
            1,
        );
        producer.poll_parser(&mut parser).unwrap();
        assert!(!stream.is_end_of_stream());
        let chunk = stream.get_chunk().unwrap();
        assert!(stream.is_end_of_stream());
        assert_eq!(chunk.request_id, 5);
        stream.release_chunk(chunk.handle);
    }

    #[test]
    fn actual_pump_accounts_and_releases_whole_strides_without_deadlock() {
        let mut bytes = record(10, 128, 0x10);
        bytes.extend(record(11, 256, 0x20));
        let source = Arc::new(MemorySource::new(0, bytes));
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "two.xsf", 0, 0x200).unwrap();
        let mut parser = pump(stream, 0x200, 2);

        producer.poll_parser(&mut parser).unwrap();
        assert_eq!(
            parser.stream().snapshot(),
            MountedCollectionStreamSnapshot {
                queued_chunks: 2,
                retained_chunks: 2,
                retained_stride_bytes: 384,
                accounted_stream_read_bytes: 384,
                producer_finished: true,
                ..MountedCollectionStreamSnapshot::default()
            }
        );
        let update = parser.update().unwrap();
        assert_eq!(update.chunks_observed, 2);
        assert_eq!(update.chunks_released_without_work, 2);
        assert!(update.became_complete);
        let snapshot = parser.stream().snapshot();
        assert_eq!(snapshot.retained_chunks, 0);
        assert_eq!(snapshot.retained_stride_bytes, 0);
        assert_eq!(snapshot.released_chunks, 2);
        assert_eq!(snapshot.accounted_stream_read_bytes, 384);
    }

    #[test]
    fn chunk_is_arc_backed_and_exposes_checked_payload_range() {
        let source = Arc::new(MemorySource::new(0, record(12, 128, 0x6b)));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(source, "arc.xsf", 0, 8).unwrap();
        let mut parser = pump(
            MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
            8,
            1,
        );
        producer.poll_parser(&mut parser).unwrap();
        let chunk = stream.get_chunk().unwrap();
        assert_eq!(chunk.handle.sequence(), 0);
        assert_eq!(chunk.handle.stride(), 128);
        assert_eq!(Arc::strong_count(chunk.handle.backing()), 1);
        assert_eq!(
            chunk.handle.payload_bytes(24, 104),
            Some(&vec![0x6b; 104][..])
        );
        assert_eq!(chunk.handle.payload_bytes(120, 9), None);
        stream.release_chunk(chunk.handle);
    }

    #[test]
    fn owned_block_view_preserves_bytes_and_provenance_without_release_authority() {
        let source = Arc::new(MemorySource::new(0x1000, record(12, 128, 0x6b)));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(source, "view.xsf", 0x1000, 8).unwrap();
        let mut parser = pump(
            MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
            8,
            1,
        );
        producer.poll_parser(&mut parser).unwrap();
        let chunk = stream.get_chunk().unwrap();
        let view = chunk.handle.owned_block_view();
        let clone = view.clone();

        assert_eq!(Arc::strong_count(view.backing()), 3);
        assert_eq!(view.bytes(), chunk.handle.bytes());
        assert_eq!(view.payload_bytes(24, 104), Some(&vec![0x6b; 104][..]));
        assert_eq!(view.payload_bytes(120, 9), None);
        assert_eq!(view.source_range(), 0x1000..0x1080);
        assert_eq!(view.sequence(), chunk.handle.sequence());
        assert_eq!(view.stride(), chunk.handle.stride());

        stream.release_chunk(chunk.handle);
        assert_eq!(stream.snapshot().released_chunks, 1);
        assert_eq!(stream.snapshot().retained_chunks, 0);
        assert_eq!(clone.bytes(), view.bytes());
        drop(clone);
        drop(view);
        assert_eq!(stream.snapshot().released_chunks, 1);
    }

    #[test]
    fn source_failure_and_parser_failure_are_sticky_first_faults() {
        let source = Arc::new(MemorySource {
            fail_at_call: Some(1),
            ..MemorySource::new(0, record(1, 128, 0))
        });
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "fail.xsf", 0, 1).unwrap();
        let mut parser = pump(stream, 1, 1);
        let first = producer.poll_parser(&mut parser).unwrap_err();
        let second = producer.poll_parser(&mut parser).unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(&*first, MountedCollectionProducerError::Source(_)));

        let source = Arc::new(MemorySource::new(0, record(2, 128, 0)));
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "parser.xsf", 0, 2).unwrap();
        let mut parser = pump(stream, 2, 1);
        producer.buffer = record(2, 128, 0);
        producer.buffer[16..20].copy_from_slice(&0_u32.to_be_bytes());
        producer.logical_len = Some(128);
        producer.storage = Some(LogicalEntryStorage::Raw);
        producer.read_cursor = 0;
        let first = producer.poll_parser(&mut parser).unwrap_err();
        let second = producer.poll_parser(&mut parser).unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(&*first, MountedCollectionProducerError::Parser(_)));
        assert!(parser.stream().snapshot().producer_failed);
    }

    /// A not-ready length probe (the entry's `0..0` read) answers `Reading`
    /// without latching, however often it repeats; the load completes once
    /// the source stops blocking.
    #[test]
    fn not_ready_probe_polls_reading_without_latching_until_ready() {
        let source = Arc::new(MemorySource {
            not_ready_calls: Some(0..2),
            ..MemorySource::new(0, record(3, 128, 0x77))
        });
        let (mut producer, stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "probe.xsf", 0, 9).unwrap();
        let mut parser = pump(stream, 9, 1);
        assert_eq!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Reading
        );
        assert_eq!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Reading
        );
        assert_eq!(source.calls(), vec![0..0, 0..0]);
        assert!(producer.terminal_error().is_none());
        let MountedCollectionProducerPoll::Stage(stage) =
            producer.poll_parser(&mut parser).unwrap()
        else {
            panic!("the read completes once the source stops blocking");
        };
        assert!(stage.finished);
        assert_eq!(source.calls().last().cloned(), Some(0..128));
    }

    /// A not-ready stage read answers `Reading` without latching, and the
    /// next poll recomputes the same range from the cursor.
    #[test]
    fn not_ready_stage_recomputes_the_same_range_without_latching() {
        let source = Arc::new(MemorySource {
            not_ready_calls: Some(1..3),
            ..MemorySource::new(0, record(4, 128, 0x88))
        });
        let (mut producer, stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "stage.xsf", 0, 10).unwrap();
        let mut parser = pump(stream, 10, 1);
        assert_eq!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Reading
        );
        assert_eq!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Reading
        );
        assert_eq!(
            source.calls(),
            vec![0..0, 0..128, 0..128],
            "the probe ran once; each retry recomputed the same stage range"
        );
        assert!(producer.terminal_error().is_none());
        assert!(matches!(
            producer.poll_parser(&mut parser).unwrap(),
            MountedCollectionProducerPoll::Stage(stage) if stage.finished
        ));
    }

    /// The stream reader's not-ready stage re-queues the same stage and keeps
    /// reporting `Reading`; the background load completes without latching.
    #[test]
    fn not_ready_background_stage_requeues_until_ready() {
        let source = Arc::new(MemorySource {
            not_ready_calls: Some(0..2),
            ..MemorySource::new(0, record(5, 128, 0x99))
        });
        let (mut producer, stream) =
            MountedCollectionProducer::new(Arc::clone(&source), "background.xsf", 0, 11).unwrap();
        producer.background = true;
        let mut parser = pump(stream, 11, 1);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match producer.poll_parser(&mut parser).unwrap() {
                MountedCollectionProducerPoll::Reading => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for the background stage to become ready"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                MountedCollectionProducerPoll::Stage(stage) => {
                    assert!(stage.finished);
                    break;
                }
                MountedCollectionProducerPoll::Finished => {
                    panic!("the producer finished without reading a stage");
                }
            }
        }
        assert!(producer.terminal_error().is_none());
        assert!(source.calls().iter().any(|range| range == &(0..128)));
    }

    #[test]
    fn truncated_eof_is_typed_and_does_not_fabricate_native_fallback_chunk() {
        let source = Arc::new(MemorySource::new(0, vec![0; 24]));
        let (mut producer, mut stream) =
            MountedCollectionProducer::new(source, "short.xsf", 0, 1).unwrap();
        let mut parser = pump(
            MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
            1,
            1,
        );
        let error = producer.poll_parser(&mut parser).unwrap_err();
        assert!(matches!(
            &*error,
            MountedCollectionProducerError::UnexpectedEndOfEntry { available: 24 }
        ));
        assert!(stream.get_chunk().is_none());
        assert!(!stream.is_end_of_stream());
    }

    #[test]
    fn source_contract_changes_latch_without_publication() {
        for source in [
            MemorySource {
                wrong_len_at_call: Some(1),
                ..MemorySource::new(0, record(1, 128, 0))
            },
            MemorySource {
                changed_storage_at_call: Some(1),
                ..MemorySource::new(0, record(1, 128, 0))
            },
            MemorySource {
                changed_logical_len_at_call: Some(1),
                ..MemorySource::new(0, record(1, 128, 0))
            },
        ] {
            let (mut producer, stream) =
                MountedCollectionProducer::new(Arc::new(source), "bad-source.xsf", 0, 1).unwrap();
            let mut parser = pump(stream, 1, 1);
            assert!(producer.poll_parser(&mut parser).is_err());
            assert_eq!(parser.stream().snapshot().queued_chunks, 0);
            assert!(parser.stream().snapshot().producer_failed);
        }
    }

    #[test]
    fn payload_offset_beyond_entry_and_max_u64_eof_are_safe_errors() {
        let source = Arc::new(MemorySource::new(0, vec![0; 64]));
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "offset.xsf", 65, 1).unwrap();
        let mut parser = pump(stream, 1, 1);
        assert!(matches!(
            &*producer.poll_parser(&mut parser).unwrap_err(),
            MountedCollectionProducerError::PayloadOffsetOutsideEntry {
                offset: 65,
                logical_len: 64
            }
        ));

        let source = Arc::new(MemorySource::new(u64::MAX - 3, vec![1, 2, 3]));
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "max.xsf", u64::MAX, 2).unwrap();
        let mut parser = pump(stream, 2, 1);
        assert!(matches!(
            &*producer.poll_parser(&mut parser).unwrap_err(),
            MountedCollectionProducerError::UnexpectedEndOfEntry { available: 0 }
        ));
    }

    #[test]
    fn mount_set_is_the_production_range_source_and_missing_file_is_sticky() {
        let mounts = Arc::new(MountSet::new());
        let (mut producer, stream) =
            MountedCollectionProducer::new(mounts, "missing.xsf", 0, 1).unwrap();
        let mut parser = pump(stream, 1, 1);
        let first = producer.poll_parser(&mut parser).unwrap_err();
        let second = producer.poll_parser(&mut parser).unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(&*first, MountedCollectionProducerError::Source(_)));
    }

    #[test]
    fn polling_does_not_hold_shared_lock_across_parser_accounting() {
        static CALLBACKS: AtomicUsize = AtomicUsize::new(0);

        struct LockCheckingParser {
            stream: MountedCollectionChunkStream,
        }

        impl CollectionRawParser for LockCheckingParser {
            type Error = std::convert::Infallible;

            fn needs_asset_header(&self) -> bool {
                true
            }

            fn parse_available(
                &mut self,
                _available: u32,
                _record: Option<&CollectionAssetRecord>,
            ) -> CollectionLoadPumpResult<CollectionParseDecision, Self::Error> {
                let guard = self
                    .stream
                    .shared
                    .try_lock()
                    .expect("producer held shared lock across parser callback");
                drop(guard);
                CALLBACKS.fetch_add(1, Ordering::Relaxed);
                Ok(CollectionParseDecision::ChunkEndRequest { block_size: 128 })
            }
        }

        let source = Arc::new(MemorySource::new(0, record(1, 128, 0)));
        let (mut producer, stream) =
            MountedCollectionProducer::new(source, "lock.xsf", 0, 1).unwrap();
        let mut parser = LockCheckingParser {
            stream: MountedCollectionChunkStream {
                shared: Arc::clone(&stream.shared),
            },
        };
        producer.poll_parser(&mut parser).unwrap();
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
    }

    fn source_ids(stream: &mut MountedCollectionChunkStream) -> Vec<u64> {
        let mut ids = Vec::new();
        while let Some(chunk) = stream.get_chunk() {
            ids.push(chunk.record.asset_id);
            stream.release_chunk(chunk.handle);
        }
        ids
    }
}
