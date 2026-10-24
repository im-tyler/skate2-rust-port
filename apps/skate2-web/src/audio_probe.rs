//! Milestone six's audio proof (`docs/WEB_PORT_M6_PLAN.md`, phase 2): the
//! pause-menu press sound rendered in the tab, through the same loader and
//! the same oracle script the pc corpus test pins, must reproduce the
//! recorded native hash byte for byte.
//!
//! The tab opens the tables of `skate2_audio::load::ARCHIVES` and `db.big`
//! over ranges, makes exactly the entries the loader reads resident as
//! stored spans (`load::wanted_entries`, plus `skater.vlt` and the `.bin`s it
//! names), and mounts them with no miss handler: a read the plan did not
//! name is a hard error, not a fetch. The streams are
//! `oracle::AbsentStreams`, counted. Everything after the fetch is
//! host-neutral and also runs natively in the ignored corpus test below —
//! the tab's exact configuration minus the transport.

use skate2_audio::{
    load,
    oracle::{self, AbsentStreams},
    rwac::stream_pool::StreamFiles,
};
use skate2_formats::{
    big::BigArchive,
    mount::{ArchiveRanges, MountSet, MountedBig},
    resident_spans::ResidentSpans,
};
use std::{
    collections::BTreeMap,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

/// The vault archive's mount path.
pub(crate) const DB_BIG: &str = "data/db/db.big";
/// The vault the audio loader reads (`load_system`'s `AttribDb::load`).
const SKATER_VLT: &str = "data\\db\\skater.vlt";

/// The mount path of one `data\audio` archive.
pub(crate) fn audio_path(name: &str) -> String {
    format!("data/audio/{name}")
}

/// One archive of the probe: its table and the stored spans made resident,
/// with no miss handler.
pub(crate) struct ProbeArchive {
    mount_path: String,
    archive: BigArchive,
    spans: Arc<ResidentSpans>,
}

impl ProbeArchive {
    pub(crate) fn new(mount_path: &str, archive: BigArchive) -> Self {
        Self {
            mount_path: mount_path.to_owned(),
            spans: Arc::new(ResidentSpans::new(mount_path)),
            archive,
        }
    }

    /// Makes the stored span beginning at `offset` resident.
    pub(crate) fn insert(&self, offset: u64, stored: Vec<u8>) {
        self.spans.insert(offset, stored);
    }

    fn mounted(&self) -> MountedBig {
        MountedBig::with_storage(
            PathBuf::from(self.mount_path.replace('/', "\\")),
            PathBuf::from(self.mount_path.replace(".big", ".bh")),
            self.archive.clone(),
            Arc::clone(&self.spans) as Arc<dyn ArchiveRanges>,
        )
    }
}

/// The stored ranges of the entries `skate2_audio::load::read_inputs` reads
/// from the `data\audio` archive `name`.
pub(crate) fn audio_ranges(name: &str, archive: &BigArchive) -> Vec<Range<u64>> {
    load::wanted_entries(name, archive)
        .into_iter()
        .map(|entry| entry.offset..entry.offset + entry.stored_size)
        .collect()
}

/// `skater.vlt`'s stored range in `db.big`.
pub(crate) fn vault_range(db: &ProbeArchive) -> Result<Range<u64>, String> {
    stored_range(db, SKATER_VLT)
}

/// The stored ranges of the `.bin`s `skater.vlt` names, once the vault's own
/// span is resident — the dependencies `AttribDb::load` reads next.
pub(crate) fn vault_dependency_ranges(db: &ProbeArchive) -> Result<Vec<Range<u64>>, String> {
    let vlt = db
        .mounted()
        .read_path(SKATER_VLT)
        .map_err(|error| format!("{SKATER_VLT}: {error}"))?;
    let metadata = skate2_formats::attrib::AttributeVaultMetadata::parse(&vlt)
        .map_err(|error| format!("{SKATER_VLT}: {error}"))?;
    metadata
        .dependencies
        .iter()
        .map(|dependency| stored_range(db, &format!("data\\db\\{}", dependency.name)))
        .collect()
}

fn stored_range(archive: &ProbeArchive, path: &str) -> Result<Range<u64>, String> {
    let entry = archive
        .archive
        .find_path(path)
        .ok_or_else(|| format!("{}: no {path}", archive.mount_path))?;
    Ok(entry.offset..entry.offset + entry.stored_size)
}

/// The probe's `data\audio` files: the span-mounted archives and the loose
/// files read whole.
struct SpanAudio<'a> {
    archives: &'a [ProbeArchive],
    loose: &'a BTreeMap<String, Vec<u8>>,
}

impl load::AudioFiles for SpanAudio<'_> {
    fn archive(&self, name: &str) -> Result<MountedBig, String> {
        let path = audio_path(name);
        self.archives
            .iter()
            .find(|archive| archive.mount_path == path)
            .map(ProbeArchive::mounted)
            .ok_or_else(|| format!("{name}: not fetched"))
    }

    fn loose(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.loose.get(name).cloned())
    }
}

/// [`AbsentStreams`], with every call counted: the oracle's claim is that no
/// stream byte exists on either host, and the counts show what asked.
#[derive(Default)]
struct CountedAbsentStreams {
    sizes: AtomicU32,
    reads: AtomicU32,
}

impl StreamFiles for CountedAbsentStreams {
    fn size(&self, path: &str) -> Option<u64> {
        self.sizes.fetch_add(1, Ordering::Relaxed);
        AbsentStreams.size(path)
    }

    fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> bool {
        self.reads.fetch_add(1, Ordering::Relaxed);
        AbsentStreams.read_at(path, offset, buf)
    }
}

/// One oracle render's evidence.
#[derive(Debug)]
pub(crate) struct OracleRun {
    pub(crate) hash: u64,
    pub(crate) frames: usize,
    /// `StreamFiles::size` calls (each answered absent).
    pub(crate) stream_sizes: u32,
    /// `StreamFiles::read_at` calls: must be zero — `size` fails first.
    pub(crate) stream_reads: u32,
    /// Reads outside the resident spans, across every archive.
    pub(crate) misses: usize,
}

/// Everything the loader reads, resident: the span-mounted `data\audio`
/// archives, `db.big` with the vault's spans, and the loose files.
pub(crate) struct AudioBytes {
    pub(crate) audio: Vec<ProbeArchive>,
    pub(crate) db: ProbeArchive,
    pub(crate) loose: BTreeMap<String, Vec<u8>>,
}

/// An audio system built from [`AudioBytes`] with the oracle's seeds and
/// counted absent streams, and its pause-menu press key.
pub(crate) struct Built {
    pub(crate) system: skate2_audio::system::AudioSystem,
    pub(crate) key: u64,
    streams: Arc<CountedAbsentStreams>,
}

/// Builds the audio system through `skate2_audio::load`: `read_inputs` over
/// the span mounts, `AttribDb::load` over the span-mounted `db.big`.
pub(crate) fn build(bytes: &AudioBytes) -> Result<Built, String> {
    let streams = Arc::new(CountedAbsentStreams::default());
    let inputs = load::read_inputs(
        &SpanAudio {
            archives: &bytes.audio,
            loose: &bytes.loose,
        },
        Arc::clone(&streams) as Arc<dyn StreamFiles>,
    )?;
    let mut mounts = MountSet::new();
    mounts.push(bytes.db.mounted());
    let vault = skate2_core::challenge::attrib_db::AttribDb::load(&mounts, &[SKATER_VLT])
        .map_err(|error| error.to_string())?;
    let (system, globals) = load::load_with_seeds(inputs, Box::new(vault), oracle::ORACLE_SEEDS)?;
    let key = *globals
        .get("FE_SOUND_PAUSE_MENU_PRESS")
        .ok_or("skater.vlt has no FE_SOUND_PAUSE_MENU_PRESS")?;
    Ok(Built {
        system,
        key,
        streams,
    })
}

/// [`build`], then the oracle script's render.
pub(crate) fn run_oracle(bytes: &AudioBytes) -> Result<OracleRun, String> {
    let mut built = build(bytes)?;
    let (hash, frames) = oracle::pause_menu_press_render(&mut built.system, built.key);
    let misses = bytes
        .audio
        .iter()
        .chain(std::iter::once(&bytes.db))
        .map(|archive| archive.spans.misses().len())
        .sum();
    Ok(OracleRun {
        hash,
        frames,
        stream_sizes: built.streams.sizes.load(Ordering::Relaxed),
        stream_reads: built.streams.reads.load(Ordering::Relaxed),
        misses,
    })
}

/// The panel's verdict for one run.
pub(crate) fn verdict(run: &OracleRun) -> String {
    let matched = run.hash == oracle::PAUSE_MENU_PRESS_HASH;
    let clean = run.stream_reads == 0 && run.misses == 0;
    if matched && clean {
        format!(
            "audio probe: {} frames, hash matches native ({:#018x}); {} stream opens answered absent, 0 stream reads, 0 span misses",
            run.frames, run.hash, run.stream_sizes
        )
    } else {
        format!(
            "audio probe: MISMATCH — tab {:#018x}, native {:#018x} ({}); {} frames, {} stream reads, {} span misses",
            run.hash,
            oracle::PAUSE_MENU_PRESS_HASH,
            oracle::PAUSE_MENU_PRESS_PLATFORM,
            run.frames,
            run.stream_reads,
            run.misses
        )
    }
}

/// The tab's driver: [`fetch`], then [`run_oracle`]. Progress lands in `line`.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn run_audio_probe<S: crate::game_files::ByteSource + ?Sized>(
    source: &S,
    line: &web_sys::Element,
) -> String {
    let run = async {
        let started = seconds_now();
        let (bytes, fetched) = fetch(source, line).await?;
        let fetch_seconds = seconds_now() - started;
        line.set_text_content(Some("audio probe: rendering…"));
        yield_to_event_loop().await;
        let render_start = seconds_now();
        let run = run_oracle(&bytes)?;
        Ok::<_, String>(format!(
            "{}; fetched {} in {fetch_seconds:.1}s, built and rendered in {:.1}s",
            verdict(&run),
            format_mib(fetched),
            seconds_now() - render_start
        ))
    };
    run.await
        .unwrap_or_else(|problem| format!("audio probe: {problem}"))
}

/// The loader's bytes over the page's source: the tables of
/// `load::ARCHIVES` and `db.big` over ranges, the planned spans, the loose
/// files whole. Returns them and the byte count fetched.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn fetch<S: crate::game_files::ByteSource + ?Sized>(
    source: &S,
    line: &web_sys::Element,
) -> Result<(AudioBytes, u64), String> {
    use crate::game_files::RemoteArchive;
    let mut fetched = 0_u64;
    let mut audio = Vec::new();
    for name in load::ARCHIVES {
        let path = audio_path(name);
        let remote = RemoteArchive::open(source, &path).await?;
        let archive = ProbeArchive::new(&path, remote.archive);
        for range in audio_ranges(name, &archive.archive) {
            fetched += fetch_span(source, &archive, range).await?;
        }
        line.set_text_content(Some(&format!(
            "audio: fetching… {name} ({})",
            format_mib(fetched)
        )));
        audio.push(archive);
    }
    let db = ProbeArchive::new(DB_BIG, RemoteArchive::open(source, DB_BIG).await?.archive);
    fetched += fetch_span(source, &db, vault_range(&db)?).await?;
    for range in vault_dependency_ranges(&db)? {
        fetched += fetch_span(source, &db, range).await?;
    }
    let mut loose = BTreeMap::new();
    for name in load::loose_files() {
        let path = audio_path(name);
        // The loose grains are optional (a dump without them loses two wheel
        // sounds); the mix map and emitter data are required, and their
        // absence surfaces as the loader's error.
        if !source.present(&path).await? {
            continue;
        }
        let bytes = source.read(&path).await?;
        fetched += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        loose.insert(name.to_owned(), bytes);
    }
    Ok((AudioBytes { audio, db, loose }, fetched))
}

/// Fetches one stored span and makes it resident; returns its size.
#[cfg(target_arch = "wasm32")]
async fn fetch_span<S: crate::game_files::ByteSource + ?Sized>(
    source: &S,
    archive: &ProbeArchive,
    range: Range<u64>,
) -> Result<u64, String> {
    let bytes = source
        .read_range(&archive.mount_path, range.clone())
        .await?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    archive.insert(range.start, bytes);
    Ok(size)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn seconds_now() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or_else(
            || js_sys::Date::now() / 1000.0,
            |performance| performance.now() / 1000.0,
        )
}

/// A `setTimeout 0` yield, so the panel paints before the render.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn yield_to_event_loop() {
    let promise = js_sys::Promise::new(
        &mut |resolve: js_sys::Function, _reject: js_sys::Function| {
            if let Some(window) = web_sys::window() {
                window.set_timeout_with_callback(&resolve).ok();
            }
        },
    );
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// A byte count as `MiB` with one decimal, for the panel.
#[cfg(target_arch = "wasm32")]
pub(crate) fn format_mib(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    format!("{}.{} MiB", bytes / MIB, bytes % MIB * 10 / MIB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    fn table(path: &std::path::Path) -> BigArchive {
        MountedBig::open(path, false).unwrap().archive().clone()
    }

    fn read_range(path: &std::path::Path, range: Range<u64>) -> Vec<u8> {
        let mut file = std::fs::File::open(skate2_formats::host_path::on_disk(path)).unwrap();
        file.seek(SeekFrom::Start(range.start)).unwrap();
        let mut bytes = vec![0; usize::try_from(range.end - range.start).unwrap()];
        file.read_exact(&mut bytes).unwrap();
        bytes
    }

    /// The tab's configuration on native: only the planned spans resident, no
    /// miss handler, the same `run_oracle` — the recorded hash, zero misses
    /// and zero stream reads. This is what proves `wanted_entries` complete.
    #[test]
    #[ignore = "requires external SKATE2_GAME_ROOT"]
    fn the_planned_spans_render_the_native_oracle() {
        let game = PathBuf::from(std::env::var_os("SKATE2_GAME_ROOT").unwrap());
        let mut audio = Vec::new();
        for name in load::ARCHIVES {
            let path = audio_path(name);
            let archive = ProbeArchive::new(&path, table(&game.join(&path)));
            for range in audio_ranges(name, &archive.archive) {
                archive.insert(range.start, read_range(&game.join(&path), range));
            }
            audio.push(archive);
        }
        let db = ProbeArchive::new(DB_BIG, table(&game.join(DB_BIG)));
        let range = vault_range(&db).unwrap();
        db.insert(range.start, read_range(&game.join(DB_BIG), range));
        for range in vault_dependency_ranges(&db).unwrap() {
            db.insert(range.start, read_range(&game.join(DB_BIG), range));
        }
        let mut loose = BTreeMap::new();
        for name in load::loose_files() {
            if let Some(bytes) =
                skate2_formats::host_path::read_if_present(&game.join(audio_path(name))).unwrap()
            {
                loose.insert(name.to_owned(), bytes);
            }
        }
        let run = run_oracle(&AudioBytes { audio, db, loose }).unwrap();
        eprintln!("{}", verdict(&run));
        assert_eq!(run.misses, 0, "a read outside the planned spans");
        assert_eq!(run.stream_reads, 0, "a stream byte was asked for");
        assert_eq!(run.hash, oracle::PAUSE_MENU_PRESS_HASH);
    }
}
