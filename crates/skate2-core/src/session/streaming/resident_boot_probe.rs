//! The resident boot probe: the exact streaming sequence a browser session
//! boot performs — the BAM world stream to its finalized bounds, then one
//! resident Simulation collection (`docs/WEB_PORT_M5_PLAN.md`, phase 3) —
//! behind one non-blocking `poll`, reporting a summary that a native
//! threaded run and a native threadless run must produce identically
//! before the browser is involved at all.
//!
//! PC port addition for the browser build; the streaming it drives is the
//! production machinery, unchanged.

use crate::{
    collection_resident_streamer::CollectionId,
    game::{WorldStreamPoll, WorldStreamSpec},
    input_cvar_runtime::InputCvarReferences,
    selected_resident_startup::SelectedResidentStartup,
    selected_world_stream::SelectedWorldStream,
    world_stream_catalog::WorldStreamCatalog,
    world_stream_runtime::WorldStreamEffects,
};
use skate2_formats::mount::MountSet;
use std::{
    fmt::Debug,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

/// The grind corpus's resident collection in the BAM world stream
/// (`physical_player_grind_corpus`), pinned as the probe's one load.
pub const BOOT_PROBE_COLLECTION: u64 = 0x823c_4449_fb31_63b4;

/// What one boot's streaming produced. Every field is order-stable: counts,
/// raw bounds bits, and lengths, never iteration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentBootReport {
    /// How many `WorldStreamEffects::add_high_collection` calls the stream
    /// made.
    pub high_collections: u32,
    /// The finalized world bounds' raw `f32` bits.
    pub bounds_min: [u32; 3],
    pub bounds_max: [u32; 3],
    /// How many arena plans the resident collection published.
    pub resident_plans: usize,
    /// The resident asset services' `(resident, retained)` counts.
    pub retained_counts: (usize, usize),
}

/// One poll of the boot: which owner is current, and the report when both
/// finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResidentBootPoll {
    /// The BAM world stream is still polling.
    Streaming,
    /// The stream finalized; the resident collection is loading.
    Resident,
    /// Both finished; the report is comparable across hosts.
    Done(ResidentBootReport),
}

/// The probe's `WorldStreamEffects`: high-collection calls counted through a
/// shared counter, so the stream can own the effects while the probe reads
/// them for the report.
#[derive(Default)]
struct ProbeEffects {
    high_collections: Arc<AtomicU32>,
}

impl WorldStreamEffects for ProbeEffects {
    fn add_high_collection(&mut self, _id: u64, _x: u32, _z: u32) {
        self.high_collections.fetch_add(1, Ordering::Relaxed);
    }

    fn finalize_world_bounds(&mut self) {}
}

/// Drives the two streaming owners of a boot. Never blocks, never sleeps,
/// never reads the clock: the host decides pacing by how often it polls.
pub struct ResidentBootProbe {
    mounts: Arc<MountSet>,
    stream: SelectedWorldStream<ProbeEffects>,
    high_collections: Arc<AtomicU32>,
    resident: Option<SelectedResidentStartup>,
    report: Option<ResidentBootReport>,
    stream_polls: u32,
    resident_polls: u32,
}

impl ResidentBootProbe {
    /// Mounts the BAM world stream over `mounts`, exactly as the corpus
    /// harness constructs it. Call
    /// [`crate::host_thread::disable_worker_threads`] first for the
    /// threadless configuration.
    ///
    /// # Errors
    ///
    /// Returns the failing stage as a `Debug` string; nothing has started.
    pub fn new(mounts: Arc<MountSet>) -> Result<Self, String> {
        let cvars = InputCvarReferences::new_static_initialized();
        crate::input_cvar_runtime::InputCvarManager::new()
            .try_link(&cvars)
            .map_err(|error| format!("cvar link: {error:?}"))?;
        let high_collections = Arc::new(AtomicU32::new(0));
        let spec =
            WorldStreamSpec::derive("BAM", None).map_err(|error| format!("spec: {error:?}"))?;
        let stream = SelectedWorldStream::try_new(
            Arc::clone(&mounts),
            WorldStreamCatalog::new(),
            cvars,
            spec,
            ProbeEffects {
                high_collections: Arc::clone(&high_collections),
            },
        )
        .map_err(|error| format!("world stream: {error:?}"))?;
        Ok(Self {
            mounts,
            stream,
            high_collections,
            resident: None,
            report: None,
            stream_polls: 0,
            resident_polls: 0,
        })
    }

    /// One poll of whichever owner is current. After [`Self::poll`] returns
    /// [`ResidentBootPoll::Done`], further polls return the same report.
    ///
    /// # Errors
    ///
    /// Returns the failing owner's error as a `Debug` string; the probe
    /// keeps its state, and the host decides whether to stop.
    pub fn poll(&mut self) -> Result<ResidentBootPoll, String> {
        if let Some(report) = &self.report {
            return Ok(ResidentBootPoll::Done(report.clone()));
        }
        let Some(mut resident) = self.resident.take() else {
            self.stream_polls += 1;
            return match self
                .stream
                .try_poll()
                .map_err(|error| format!("poll: {error:?}"))?
            {
                WorldStreamPoll::Pending => Ok(ResidentBootPoll::Streaming),
                WorldStreamPoll::CompletedThisPoll => {
                    let mut started = SelectedResidentStartup::new(
                        Arc::clone(&self.mounts),
                        self.stream.catalog.clone(),
                    );
                    started
                        .request(CollectionId(BOOT_PROBE_COLLECTION))
                        .map_err(|error| format!("request: {error:?}"))?;
                    self.resident = Some(started);
                    Ok(ResidentBootPoll::Resident)
                }
            };
        };
        self.resident_polls += 1;
        let ready = resident
            .poll()
            .map_err(|error| format!("resident: {error:?}"))?;
        if !ready {
            self.resident = Some(resident);
            return Ok(ResidentBootPoll::Resident);
        }
        let bounds = self
            .stream
            .bounds
            .read()
            .map_err(|error| format!("bounds: {error:?}"))?;
        let report = ResidentBootReport {
            high_collections: self.high_collections.load(Ordering::Relaxed),
            bounds_min: [bounds.min[0], bounds.min[1], bounds.min[2]],
            bounds_max: [bounds.max[0], bounds.max[1], bounds.max[2]],
            resident_plans: resident.plans().map_or(0, <[_]>::len),
            retained_counts: resident.assets().retained_counts(),
        };
        // The resident owner stays alive for the probe's lifetime, as the
        // corpus retains its stream: the loaded collection is not unloaded.
        self.resident = Some(resident);
        self.report = Some(report.clone());
        Ok(ResidentBootPoll::Done(report))
    }

    /// `(stream polls, resident polls)` — host diagnostics; not part of the
    /// report, which must not depend on poll cadence.
    #[must_use]
    pub fn polls(&self) -> (u32, u32) {
        (self.stream_polls, self.resident_polls)
    }
}
