//! In-memory stored spans of one archive's data file: the browser build's
//! [`crate::mount::ArchiveRanges`] implementation (`docs/WEB_PORT_M5_PLAN`).
//! The web app prefetches the spans a recorded boot manifest names before
//! core asks. A read outside the resident spans is a **not-ready** answer:
//! `ErrorKind::WouldBlock` with the miss logged, and — when the host
//! installed one — a miss handler called outside the span lock, so the host
//! can fetch the span on demand (`docs/WEB_PORT_M5A_PLAN`). A read outside
//! the resident spans asks to be made resident and retried.

use std::{
    collections::BTreeMap,
    fmt, io,
    ops::Bound,
    sync::{Arc, Mutex},
};

/// The stored spans of one archive, keyed by their start offset, plus the
/// log of reads that fell outside them.
pub struct ResidentSpans {
    /// The archive's data-file name, for miss messages.
    archive: String,
    state: Mutex<SpansState>,
    /// The host's on-demand fetch kick, called once per miss, outside the
    /// span lock. Must not read this span set (the lock is not re-entrant
    /// on the browser's single thread) and must not panic.
    miss_handler: Option<Box<dyn Fn(u64, usize) + Send + Sync>>,
}

impl fmt::Debug for ResidentSpans {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentSpans")
            .field("archive", &self.archive)
            .field("handler", &self.miss_handler.as_ref().map_or(0, |_| 1))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct SpansState {
    spans: BTreeMap<u64, Arc<[u8]>>,
    misses: Vec<(u64, usize)>,
}

impl Default for ResidentSpans {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl ResidentSpans {
    /// An empty span set for the archive named `archive`.
    #[must_use]
    pub fn new(archive: impl Into<String>) -> Self {
        Self {
            archive: archive.into(),
            state: Mutex::new(SpansState::default()),
            miss_handler: None,
        }
    }

    /// Installs the host's miss handler: called with the miss's offset and
    /// length after the miss is logged and the span lock released, so the
    /// host can fetch the span on demand. The handler must not read this
    /// span set (its lock is not re-entrant on a single thread).
    #[must_use]
    pub fn with_miss_handler(mut self, handler: Box<dyn Fn(u64, usize) + Send + Sync>) -> Self {
        self.miss_handler = Some(handler);
        self
    }

    /// Makes the stored span beginning at `offset` resident.
    pub fn insert(&self, offset: u64, stored: Vec<u8>) {
        self.lock().spans.insert(offset, Arc::from(stored));
    }

    /// Every read that fell outside the resident spans, in read order.
    pub fn misses(&self) -> Vec<(u64, usize)> {
        self.lock().misses.clone()
    }

    /// How many stored bytes are resident.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.lock()
            .spans
            .values()
            .map(|span| u64::try_from(span.len()).unwrap_or(u64::MAX))
            .sum()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SpansState> {
        // No code panics while the lock is held, but a poisoned lock must not
        // hide resident data from reads that can still be served.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl crate::mount::ArchiveRanges for ResidentSpans {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        let length = u64::try_from(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::Unsupported, "read too large"))?;
        let miss = {
            let mut state = self.lock();
            if let Ok(bytes) = Self::serving_span(&state, offset, length) {
                buffer.copy_from_slice(bytes);
                return Ok(());
            }
            // Spans are never stitched: a read crossing into the next one
            // is a miss, because only whole stored spans are resident. The
            // miss is logged under the lock…
            state.misses.push((offset, buffer.len()));
            (offset, buffer.len())
        };
        // …and the host's fetch kick fires outside it, so the handler may
        // not re-enter this span set but may take its own time.
        if let Some(handler) = &self.miss_handler {
            handler(miss.0, miss.1);
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("not resident: {} {}+{}", self.archive, miss.0, miss.1),
        ))
    }
}

impl ResidentSpans {
    /// The resident bytes at `offset..offset+length`, when one span holds
    /// the whole read; `Err(())` is a miss.
    fn serving_span(state: &SpansState, offset: u64, length: u64) -> Result<&[u8], ()> {
        let (&start, span) = state
            .spans
            .range((Bound::Unbounded, Bound::Included(offset)))
            .next_back()
            .ok_or(())?;
        let within = offset - start;
        let end = within.checked_add(length).ok_or(())?;
        let (within, end) = (usize::try_from(within), usize::try_from(end));
        let (Ok(within), Ok(end)) = (within, end) else {
            return Err(());
        };
        let bytes = span.get(within..end).ok_or(())?;
        (bytes.len() == usize::try_from(length).map_err(|_| ())?)
            .then_some(bytes)
            .ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_with(entries: &[(u64, &[u8])]) -> ResidentSpans {
        let spans = ResidentSpans::new("test.big");
        for (offset, bytes) in entries {
            spans.insert(*offset, bytes.to_vec());
        }
        spans
    }

    #[test]
    fn read_inside_one_span_serves_it_exactly() {
        let spans = spans_with(&[(16, b"abcdefgh"), (64, b"XYZW")]);
        let mut buffer = [0_u8; 4];
        crate::mount::ArchiveRanges::read_at(&spans, 18, &mut buffer).unwrap();
        assert_eq!(&buffer, b"cdef");
        crate::mount::ArchiveRanges::read_at(&spans, 64, &mut buffer).unwrap();
        assert_eq!(&buffer, b"XYZW");
        assert!(spans.misses().is_empty());
        assert_eq!(spans.resident_bytes(), 12);
    }

    #[test]
    fn read_before_the_first_span_misses() {
        let spans = spans_with(&[(16, b"abcdefgh")]);
        let mut buffer = [0_u8; 2];
        let error = crate::mount::ArchiveRanges::read_at(&spans, 0, &mut buffer).unwrap_err();
        assert_eq!(
            error.kind(),
            io::ErrorKind::WouldBlock,
            "a miss asks to be made resident and retried"
        );
        assert_eq!(spans.misses(), vec![(0, 2)]);
    }

    #[test]
    fn read_past_a_span_end_misses_and_is_never_stitched() {
        // Two adjacent spans: a read crossing the first into the second is a
        // miss, not a concatenation.
        let spans = spans_with(&[(16, b"abcd"), (20, b"efgh")]);
        let mut buffer = [0_u8; 6];
        crate::mount::ArchiveRanges::read_at(&spans, 18, &mut buffer).unwrap_err();
        assert_eq!(spans.misses(), vec![(18, 6)]);
        // The exact span still serves afterwards.
        let mut four = [0_u8; 4];
        crate::mount::ArchiveRanges::read_at(&spans, 20, &mut four).unwrap();
        assert_eq!(&four, b"efgh");
    }

    #[test]
    fn the_miss_handler_fires_outside_the_span_lock() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicU32::new(0));
        let handler_seen = Arc::clone(&seen);
        let handler_calls = Arc::clone(&calls);
        let spans = ResidentSpans::new("on-demand.big").with_miss_handler(Box::new(
            move |offset, length| {
                handler_seen.lock().unwrap().push((offset, length));
                handler_calls.fetch_add(1, Ordering::Relaxed);
            },
        ));
        let mut buffer = [0_u8; 4];
        let error = crate::mount::ArchiveRanges::read_at(&spans, 32, &mut buffer).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "one kick per miss");
        assert_eq!(*seen.lock().unwrap(), vec![(32, 4)]);
        // The miss is logged, and a repeat miss kicks again.
        let _ = crate::mount::ArchiveRanges::read_at(&spans, 32, &mut buffer);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(spans.misses(), vec![(32, 4), (32, 4)]);
    }

    #[test]
    fn empty_reads_serve_without_touching_the_spans() {
        let spans = ResidentSpans::new("empty.big");
        crate::mount::ArchiveRanges::read_at(&spans, 0, &mut []).unwrap();
        assert!(spans.misses().is_empty());
        assert_eq!(spans.resident_bytes(), 0);
    }
}
