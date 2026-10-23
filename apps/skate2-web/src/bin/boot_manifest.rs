//! The boot manifest tool (native only; `docs/WEB_PORT_M5_PLAN.md`, phase 4):
//! records every archive entry a threadless boot probe reads, proves the
//! list is closed by re-running the same probe against in-memory spans with
//! zero misses and an identical report, then writes `boot-manifest.tsv` for
//! the browser build to prefetch. Nothing from the dump itself is written —
//! offsets, sizes, paths and hashes are metadata.

#[cfg(not(target_arch = "wasm32"))]
mod native {

    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
        process::ExitCode,
        sync::{Arc, Mutex, OnceLock},
    };

    use skate2_core::resident_boot_probe::{
        ResidentBootPoll, ResidentBootProbe, ResidentBootReport,
    };
    use skate2_formats::{
        big::BigEntryKey,
        host_path,
        mount::{ArchiveRanges, MountSet, MountedBig, SKATE2_ARCHIVES, observe_reads},
        resident_spans::ResidentSpans,
    };

    /// One recorded entry read: which archive, a bijective display of the entry
    /// key (`n<name>` / `h<hash>`), and where it sits. Display-keyed because
    /// `BigEntryKey` carries no ordering.
    type ReadKey = (PathBuf, String, u64, u64);

    /// The bijective display form of an entry key.
    fn display_of(key: &BigEntryKey) -> String {
        match key {
            BigEntryKey::Name(name) => format!("n{name}"),
            BigEntryKey::Hash(hash) => format!("h{hash:010x}"),
        }
    }

    static SEEN: OnceLock<Mutex<BTreeSet<ReadKey>>> = OnceLock::new();

    /// The read observer: records every entry the probe reads. Called on the
    /// polling thread only — the tool disables worker threads first.
    fn record(read: &skate2_formats::mount::ArchiveRead<'_>) {
        let seen = SEEN.get_or_init(|| Mutex::new(BTreeSet::new()));
        if let Ok(mut seen) = seen.lock() {
            seen.insert((
                read.archive.to_owned(),
                display_of(&read.entry.key),
                read.entry.offset,
                read.entry.stored_size,
            ));
        }
    }

    pub(super) fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn run() -> Result<(), String> {
        let Some(root) = std::env::var_os("SKATE2_GAME_ROOT").map(PathBuf::from) else {
            return Err("SKATE2_GAME_ROOT names the game dump".to_owned());
        };
        // The browser's configuration: no worker threads anywhere.
        skate2_core::host_thread::disable_worker_threads();

        // 1. Record: file-backed mounts, observer installed, one probe run.
        observe_reads(record);
        let file_report = run_probe(mount_from_files(&root)?);
        let seen = SEEN
            .get()
            .and_then(|seen| seen.lock().ok().map(|seen| seen.clone()))
            .ok_or("the observer recorded nothing")?;
        eprintln!("recorded {} distinct entry reads", seen.len());

        // 2. Close: the same entries resident in memory, a fresh mount set and
        // probe, zero misses and the identical report.
        let spans_by_archive = close_over_spans(&root, &seen, &file_report)?;

        // 3. Write the manifest, deterministic: mount order, then offset.
        write_manifest(&seen, &file_report, &spans_by_archive, &root)
    }

    /// One mounted archive's resident spans for the closure proof.
    type SpanArchive = (&'static str, Arc<ResidentSpans>);

    /// Proves the recorded read set is closed: every recorded entry resident in
    /// memory, a fresh mount set built from spans alone, the same probe, zero
    /// misses, and the identical report.
    fn close_over_spans(
        root: &Path,
        seen: &BTreeSet<ReadKey>,
        file_report: &ResidentBootReport,
    ) -> Result<Vec<SpanArchive>, String> {
        let mut span_mounts = MountSet::new();
        let mut spans_by_archive: Vec<SpanArchive> = Vec::new();
        for spec in SKATE2_ARCHIVES {
            let data_path = spec.path_under(root);
            if !data_path.is_file() {
                continue;
            }
            let mounted = MountedBig::open(&data_path, spec.use_external_header)
                .map_err(|error| format!("{}: {error}", data_path.display()))?;
            let spans = Arc::new(ResidentSpans::new(spec.relative_path));
            let archive = mounted.archive().clone();
            for (archive_path, display, _, _) in seen {
                if *archive_path != data_path {
                    continue;
                }
                let entry = archive
                    .entries
                    .iter()
                    .find(|entry| display_of(&entry.key) == *display)
                    .ok_or_else(|| {
                        format!(
                            "{}: a recorded entry is absent on remount",
                            data_path.display()
                        )
                    })?;
                let stored = mounted
                    .read_stored(entry)
                    .map_err(|error| format!("{}: {error}", data_path.display()))?;
                spans.insert(entry.offset, stored);
            }
            span_mounts.push(MountedBig::with_storage(
                mounted.data_path().to_owned(),
                mounted.header_path().to_owned(),
                archive,
                spans.clone() as Arc<dyn ArchiveRanges>,
            ));
            spans_by_archive.push((spec.relative_path, spans));
        }
        let span_report = run_probe(span_mounts);
        let misses: Vec<_> = spans_by_archive
            .iter()
            .filter_map(|(name, spans)| {
                let misses = spans.misses();
                (!misses.is_empty()).then(|| format!("{name}: {misses:?}"))
            })
            .collect();
        if !misses.is_empty() {
            return Err(format!(
                "resident spans missed reads (unclosed manifest): {misses:?}"
            ));
        }
        if span_report != *file_report {
            return Err(format!(
                "span report differs from file report:\n  file: {file_report:?}\n  span: {span_report:?}"
            ));
        }
        Ok(spans_by_archive)
    }

    /// Writes the manifest, deterministic: mount order, then offset. Nothing
    /// from the dump itself — paths, keys, sizes and offsets are metadata.
    fn write_manifest(
        seen: &BTreeSet<ReadKey>,
        file_report: &ResidentBootReport,
        spans_by_archive: &[SpanArchive],
        root: &Path,
    ) -> Result<(), String> {
        let mut lines: Vec<String> = Vec::new();
        let mut total_bytes = 0_u64;
        for (relative_path, _) in spans_by_archive {
            let data_path = spec_path(root, relative_path);
            let mut archive_lines: Vec<(u64, String, u64)> = Vec::new();
            for (archive_path, display, offset, stored) in seen {
                if *archive_path != data_path {
                    continue;
                }
                let kind = match display.as_bytes().first() {
                    Some(b'n') => format!("name\t{}", &display[1..]),
                    _ => format!("hash\t{}", &display[1..]),
                };
                archive_lines.push((*offset, kind, *stored));
                total_bytes += *stored;
            }
            archive_lines.sort_unstable_by_key(|(offset, _, _)| *offset);
            for (offset, kind, stored) in archive_lines {
                lines.push(format!("{relative_path}\t{kind}\t{stored}\t# at {offset}"));
            }
        }
        let manifest = manifest_path();
        let text = format!(
            "# skate2 boot manifest v1\n# report\t{file_report:?}\n# entries\t{}\tstored_bytes\t{total_bytes}\n{}\n",
            seen.len(),
            lines.join("\n")
        );
        std::fs::write(&manifest, text)
            .map_err(|error| format!("{}: {error}", manifest.display()))?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "byte totals are far below f64 precision"
        )]
        let total_mib = total_bytes as f64 / 1024.0 / 1024.0;
        eprintln!(
            "boot manifest closed: {} entries, {total_bytes} stored bytes ({total_mib:.1} MiB) -> {}",
            seen.len(),
            manifest.display()
        );
        if total_bytes > 1024_u64 * 1024 * 1024 {
            return Err(format!(
                "manifest is {total_mib:.0} MiB, over the 1 GiB planning cap: stop and report"
            ));
        }
        Ok(())
    }

    /// The manifest lives beside the web app that embeds it.
    fn manifest_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("boot-manifest.tsv")
    }

    /// The mount path of a spec under the root, for matching recorded archives.
    fn spec_path(root: &Path, relative_path: &str) -> PathBuf {
        host_path::on_disk(&root.join(relative_path.replace('\\', std::path::MAIN_SEPARATOR_STR)))
    }

    fn mount_from_files(root: &Path) -> Result<MountSet, String> {
        let mut mounts = MountSet::new();
        for spec in SKATE2_ARCHIVES {
            let path = spec.path_under(root);
            if path.is_file() {
                mounts.push(
                    MountedBig::open(path, spec.use_external_header)
                        .map_err(|error| format!("{}: {error}", spec.relative_path))?,
                );
            }
        }
        Ok(mounts)
    }

    fn run_probe(mounts: MountSet) -> ResidentBootReport {
        let mut probe = ResidentBootProbe::new(Arc::new(mounts)).expect("the boot probe starts");
        loop {
            match probe.poll().expect("the boot probe polls") {
                ResidentBootPoll::Done(report) => return report,
                _ => std::thread::yield_now(),
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    native::main()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
