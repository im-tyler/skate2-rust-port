//! In-memory stored spans of one archive's data file: the browser build's
//! [`crate::mount::ArchiveRanges`] implementation (`docs/WEB_PORT_M5_PLAN`).
//! The web app prefetches the spans a recorded boot manifest names before
//! core asks; a read outside the resident spans is an honest error, and the
//! miss is logged so the manifest's closure can be proven.

use std::{
    collections::BTreeMap,
    io,
    ops::Bound,
    sync::{Arc, Mutex},
};

/// The stored spans of one archive, keyed by their start offset, plus the
/// log of reads that fell outside them.
#[derive(Debug, Default)]
pub struct ResidentSpans {
    /// The archive's data-file name, for miss messages.
    archive: String,
    state: Mutex<SpansState>,
}

#[derive(Debug, Default)]
struct SpansState {
    spans: BTreeMap<u64, Arc<[u8]>>,
    misses: Vec<(u64, usize)>,
}

impl ResidentSpans {
    /// An empty span set for the archive named `archive`.
    #[must_use]
    pub fn new(archive: impl Into<String>) -> Self {
        Self {
            archive: archive.into(),
            state: Mutex::new(SpansState::default()),
        }
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

    /// Records a miss and returns its error.
    fn miss(&self, state: &mut SpansState, offset: u64, length: usize) -> io::Error {
        state.misses.push((offset, length));
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("not resident: {} {offset}+{length}", self.archive),
        )
    }
}

impl crate::mount::ArchiveRanges for ResidentSpans {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        let length = u64::try_from(buffer.len())
            .map_err(|_| io::Error::new(io::ErrorKind::Unsupported, "read too large"))?;
        let mut state = self.lock();
        let Some((&start, span)) = state
            .spans
            .range((Bound::Unbounded, Bound::Included(offset)))
            .next_back()
        else {
            return Err(self.miss(&mut state, offset, buffer.len()));
        };
        let within = offset - start;
        let end = within
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "read past the span end"))?;
        // Spans are never stitched: a read crossing into the next one is a
        // miss, because the manifest named a span the boot does not read.
        let (within, end) = (usize::try_from(within), usize::try_from(end));
        let (Ok(within), Ok(end)) = (within, end) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "span bounds past memory",
            ));
        };
        let Some(bytes) = span.get(within..end) else {
            return Err(self.miss(&mut state, offset, buffer.len()));
        };
        if bytes.len() != buffer.len() {
            return Err(self.miss(&mut state, offset, buffer.len()));
        }
        buffer.copy_from_slice(bytes);
        Ok(())
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
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
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
    fn empty_reads_serve_without_touching_the_spans() {
        let spans = ResidentSpans::new("empty.big");
        crate::mount::ArchiveRanges::read_at(&spans, 0, &mut []).unwrap();
        assert!(spans.misses().is_empty());
        assert_eq!(spans.resident_bytes(), 0);
    }
}
