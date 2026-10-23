//! Milestone five's payoff (`docs/WEB_PORT_M5_PLAN.md`, phase 5): the boot
//! probe running in the tab. The committed `boot-manifest.tsv` names every
//! archive entry a boot reads; the browser fetches exactly those stored
//! spans, serves them to core through `MountedBig::with_storage` +
//! `ResidentSpans`, and drives `ResidentBootProbe` on a cooperative
//! `setTimeout` loop — no `requestAnimationFrame`, which does not tick in
//! background or headless tabs. Done means the report equals the manifest's
//! recorded native report.
//!
//! The manifest parser is pure and unit-tested on native; only the driver is
//! wasm-only.

use skate2_core::resident_boot_probe::ResidentBootReport;

/// One manifest entry: the archive it lives in and its entry key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestEntry {
    /// The mount path, backslash form (`data\content\worldbam.big`).
    pub(crate) mount_path: String,
    /// The entry's key: its virtual path, or its table hash.
    pub(crate) key: ManifestKey,
    /// The stored span's size in bytes.
    pub(crate) stored: u64,
}

/// A manifest entry's key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManifestKey {
    /// A named entry's virtual path, backslash form.
    Name(String),
    /// A hashed entry's table hash.
    Hash(u32),
}

/// The parsed manifest: the recorded native report and every entry the boot
/// reads.
#[derive(Clone, Debug)]
pub(crate) struct BootManifest {
    pub(crate) report: ResidentBootReport,
    pub(crate) entries: Vec<ManifestEntry>,
}

/// Parses the manifest the tool wrote: a `# report` header, an `# entries`
/// count header, then one `<archive>\t<name|hash>\t<value>\t<stored>` line
/// per entry (the trailing `# at <offset>` comment is the tool's, not the
/// browser's — offsets come from the archive tables).
///
/// # Errors
///
/// Returns the first structural problem, with the offending line quoted.
pub(crate) fn parse_manifest(text: &str) -> Result<BootManifest, String> {
    let mut report = None;
    let mut entries = Vec::new();
    for line in text.lines() {
        if let Some(debug) = line.strip_prefix("# report\t") {
            report = Some(parse_report(debug)?);
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let mount_path = fields
            .next()
            .ok_or_else(|| format!("manifest line without an archive: {line:?}"))?
            .to_owned();
        let kind = fields
            .next()
            .ok_or_else(|| format!("manifest line without a key kind: {line:?}"))?;
        let value = fields
            .next()
            .ok_or_else(|| format!("manifest line without a key: {line:?}"))?;
        let stored_field = fields
            .next()
            .ok_or_else(|| format!("manifest line without a size: {line:?}"))?;
        let stored: u64 = stored_field
            .parse()
            .map_err(|_| format!("manifest size is not a number: {stored_field:?}"))?;
        let key = match kind {
            "name" => ManifestKey::Name(value.to_owned()),
            "hash" => ManifestKey::Hash(
                u32::from_str_radix(value.trim_start_matches("0x"), 16)
                    .map_err(|_| format!("manifest hash is not hexadecimal: {value:?}"))?,
            ),
            other => return Err(format!("unknown manifest key kind {other:?}")),
        };
        entries.push(ManifestEntry {
            mount_path,
            key,
            stored,
        });
    }
    let report = report.ok_or("the manifest has no # report header")?;
    Ok(BootManifest { report, entries })
}

/// Parses the report header's `Debug` form. Only the manifest tool writes
/// this line, so the grammar is exactly what `ResidentBootReport`'s `Debug`
/// prints today.
fn parse_report(debug: &str) -> Result<ResidentBootReport, String> {
    let scalar = |key: &str| -> Result<u64, String> {
        let at = debug
            .find(&format!("{key}: "))
            .ok_or_else(|| format!("the report line has no {key}"))?;
        let tail = &debug[at + key.len() + 2..];
        let end = tail
            .find(|byte: char| !byte.is_ascii_digit())
            .unwrap_or(tail.len());
        tail[..end]
            .parse()
            .map_err(|_| format!("the report's {key} is not a number"))
    };
    let triple = |key: &str| -> Result<[u32; 3], String> {
        let at = debug
            .find(&format!("{key}: ["))
            .ok_or_else(|| format!("the report line has no {key}"))?;
        let tail = &debug[at..];
        let end = tail
            .find(']')
            .ok_or_else(|| format!("the report's {key} is unterminated"))?;
        let mut numbers = tail[tail.find('[').expect("checked") + 1..end].split(',');
        let mut words = [0_u32; 3];
        for word in &mut words {
            let number = numbers
                .next()
                .ok_or_else(|| format!("the report's {key} has too few numbers"))?
                .trim();
            *word = number
                .parse()
                .map_err(|_| format!("the report's {key} holds a non-number"))?;
        }
        Ok(words)
    };
    let tuple = |key: &str| -> Result<(usize, usize), String> {
        let at = debug
            .find(&format!("{key}: ("))
            .ok_or_else(|| format!("the report line has no {key}"))?;
        let tail = &debug[at..];
        let end = tail
            .find(')')
            .ok_or_else(|| format!("the report's {key} is unterminated"))?;
        let mut numbers = tail[tail.find('(').expect("checked") + 1..end].split(',');
        let mut parse = |name: &str| -> Result<usize, String> {
            let number = numbers
                .next()
                .ok_or_else(|| format!("the report's {key} has too few numbers"))?
                .trim();
            number
                .parse()
                .map_err(|_| format!("the report's {key} {name} is not a number"))
        };
        let first = parse("first")?;
        let second = parse("second")?;
        Ok((first, second))
    };
    Ok(ResidentBootReport {
        high_collections: u32::try_from(scalar("high_collections")?)
            .map_err(|_| "the report's high_collections does not fit a u32".to_owned())?,
        bounds_min: triple("bounds_min")?,
        bounds_max: triple("bounds_max")?,
        resident_plans: usize::try_from(scalar("resident_plans")?)
            .map_err(|_| "the report's resident_plans is negative".to_owned())?,
        retained_counts: tuple("retained_counts")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed manifest parses with exactly the recorded oracle.
    #[test]
    fn the_committed_manifest_parses_to_the_recorded_report() {
        let manifest = parse_manifest(include_str!("../boot-manifest.tsv")).unwrap();
        assert_eq!(
            manifest.report,
            ResidentBootReport {
                high_collections: 284,
                bounds_min: [3311468544, 3275851331, 3307372544],
                bounds_max: [1159069696, 1139775930, 1157234688],
                resident_plans: 21,
                retained_counts: (21, 80),
            }
        );
        assert_eq!(manifest.entries.len(), 12);
        assert_eq!(
            manifest
                .entries
                .iter()
                .map(|entry| entry.stored)
                .sum::<u64>(),
            2_815_040
        );
        assert_eq!(
            manifest.entries[0],
            ManifestEntry {
                mount_path: "data\\content\\worldbam.big".to_owned(),
                key: ManifestKey::Name(
                    "data\\content\\world\\stream\\BAM\\BAM_Pres.xst".to_owned()
                ),
                stored: 1_042_712,
            }
        );
    }

    #[test]
    fn a_hash_entry_parses_hexadecimal_and_names_stay_verbatim() {
        let text = "# skate2 boot manifest v1\n\
                    # report\tResidentBootReport { high_collections: 1, bounds_min: [1, 2, 3], bounds_max: [4, 5, 6], resident_plans: 7, retained_counts: (8, 9) }\n\
                    # entries\t2\tstored_bytes\t20\n\
                    data\\a.big\tname\tpath\\one.bin\t12\t# at 0\n\
                    data\\a.big\thash\t00000af1\t8\t# at 12\n";
        let manifest = parse_manifest(text).unwrap();
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(
            manifest.entries[1].key,
            ManifestKey::Hash(0xaf1),
            "hashes parse from their zero-padded hexadecimal form"
        );
    }

    #[test]
    fn structural_problems_are_named() {
        assert!(parse_manifest("no header\n").is_err());
        assert!(parse_manifest("# report\tResidentBootReport { }\n").is_err());
        assert!(parse_manifest(
            "# report\tResidentBootReport { high_collections: 1, bounds_min: [1, 2, 3], bounds_max: [4, 5, 6], resident_plans: 7, retained_counts: (8, 9) }\n\
             data\\a.big\twhat\tx\t4\n"
        )
        .is_err());
    }
}

/// The committed manifest, embedded at build time.
#[cfg(target_arch = "wasm32")]
const MANIFEST: &str = include_str!("../boot-manifest.tsv");

/// Runs the boot probe in the tab and returns the panel's final line;
/// progress lines land in `progress`.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn run_boot_probe<S: crate::game_files::ByteSource + ?Sized>(
    source: &S,
    progress: &web_sys::Element,
) -> String {
    let manifest = match parse_manifest(MANIFEST) {
        Ok(manifest) => manifest,
        Err(problem) => return format!("boot probe: the committed manifest is invalid: {problem}"),
    };
    // The browser's configuration, before anything core touches a thread.
    skate2_core::host_thread::disable_worker_threads();
    let remote = crate::game_files::RemoteMountSet::open_all(source).await;
    let (spans, resident_bytes, fetch_seconds) =
        fetch_boot_spans(source, &remote, &manifest, progress).await;
    let (report, boot_seconds, longest_poll) = drive_boot_probe(&remote, &spans, progress).await;

    let misses: Vec<String> = spans
        .values()
        .filter(|archive_spans| !archive_spans.misses().is_empty())
        .map(|archive_spans| format!("{} misses", archive_spans.misses().len()))
        .collect();
    let misses_text = if misses.is_empty() {
        "zero span misses".to_owned()
    } else {
        misses.join(", ")
    };
    let verdict = if report == manifest.report {
        "done — matches native".to_owned()
    } else {
        format!(
            "MISMATCH\n  manifest: {:?}\n  this run:  {report:?}",
            manifest.report
        )
    };
    format!(
        "boot probe: {verdict}; spans {} in {:.1}s, boot {:.1}s, longest poll {:.1} ms, {misses_text}",
        format_mib(resident_bytes),
        fetch_seconds,
        boot_seconds,
        longest_poll * 1000.0,
    )
}

/// Fetches every manifest entry's stored span over ranges: the bytes core
/// will read, resident before core asks. Progress lands in `progress`
/// between fetches; the tab stays responsive.
#[cfg(target_arch = "wasm32")]
async fn fetch_boot_spans<S: crate::game_files::ByteSource + ?Sized>(
    source: &S,
    remote: &crate::game_files::RemoteMountSet,
    manifest: &BootManifest,
    progress: &web_sys::Element,
) -> (Spans, u64, f64) {
    let mut spans = Spans::new();
    let mut resident_bytes = 0_u64;
    let fetch_start = seconds_now();
    for (index, entry) in manifest.entries.iter().enumerate() {
        let normalized = entry.mount_path.replace('\\', "/");
        let Some(&(mount_path, ref archive)) = remote
            .archives()
            .iter()
            .find(|(path, _)| *path == normalized)
        else {
            note_progress(
                progress,
                &format!(
                    "boot probe: {} is not among the mounted archives",
                    entry.mount_path
                ),
            );
            break;
        };
        let table_entry = match &entry.key {
            ManifestKey::Name(name) => archive
                .archive
                .find_path(name)
                .or_else(|| archive.archive.find_path(&name.replace('\\', "/"))),
            ManifestKey::Hash(hash) => archive.archive.find_hash(*hash),
        };
        let Some(table_entry) = table_entry else {
            let shown = match &entry.key {
                ManifestKey::Name(name) => name.clone(),
                ManifestKey::Hash(hash) => format!("{hash:#010x}"),
            };
            note_progress(
                progress,
                &format!("boot probe: {shown} is absent from this dump"),
            );
            break;
        };
        let Some(end) = table_entry.offset.checked_add(table_entry.stored_size) else {
            note_progress(progress, "boot probe: an entry's range overflows");
            break;
        };
        let bytes = match source
            .read_range(&archive.data_path, table_entry.offset..end)
            .await
        {
            Ok(bytes) => bytes,
            Err(problem) => {
                note_progress(progress, &format!("boot probe: fetch failed: {problem}"));
                break;
            }
        };
        resident_bytes += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        spans
            .entry(mount_path)
            .or_insert_with(|| Arc::new(ResidentSpans::new(mount_path.to_owned())))
            .insert(table_entry.offset, bytes);
        note_progress(
            progress,
            &format!(
                "boot probe: fetching spans… {}/{}",
                index + 1,
                manifest.entries.len()
            ),
        );
        yield_to_event_loop().await;
    }
    (spans, resident_bytes, seconds_now() - fetch_start)
}

/// Builds core's mount set over the spans and drives the probe cooperatively
/// — 8 ms polling bursts, then a `setTimeout 0` yield so the tab breathes —
/// until its report is ready.
#[cfg(target_arch = "wasm32")]
async fn drive_boot_probe(
    remote: &crate::game_files::RemoteMountSet,
    spans: &Spans,
    progress: &web_sys::Element,
) -> (ResidentBootReport, f64, f64) {
    use skate2_formats::mount::{ArchiveRanges, MountSet, MountedBig};
    let mut mounts = MountSet::new();
    for (mount_path, archive) in remote.archives() {
        let archive_spans = spans.get(mount_path).map_or_else(
            || Arc::new(ResidentSpans::new((*mount_path).to_owned())),
            Arc::clone,
        );
        mounts.push(MountedBig::with_storage(
            std::path::PathBuf::from(mount_path.replace('/', "\\")),
            std::path::PathBuf::from(mount_path.replace(".big", ".bh")),
            archive.archive.clone(),
            archive_spans as Arc<dyn ArchiveRanges>,
        ));
    }
    let mut probe = match skate2_core::resident_boot_probe::ResidentBootProbe::new(Arc::new(mounts))
    {
        Ok(probe) => probe,
        Err(problem) => {
            note_progress(progress, &format!("boot probe: start failed: {problem}"));
            return placeholder_report();
        }
    };
    let boot_start = seconds_now();
    let mut longest_poll = 0_f64;
    let report = loop {
        let burst_start = seconds_now();
        let mut done = None;
        loop {
            let poll_start = seconds_now();
            match probe.poll() {
                Ok(skate2_core::resident_boot_probe::ResidentBootPoll::Done(report)) => {
                    longest_poll = longest_poll.max(seconds_now() - poll_start);
                    done = Some(report);
                    break;
                }
                Ok(_) => {
                    longest_poll = longest_poll.max(seconds_now() - poll_start);
                    if seconds_now() - burst_start >= 0.008 {
                        break;
                    }
                }
                Err(problem) => {
                    note_progress(progress, &format!("boot probe: poll failed: {problem}"));
                    return placeholder_report();
                }
            }
        }
        if let Some(report) = done {
            break report;
        }
        let (stream_polls, resident_polls) = probe.polls();
        note_progress(
            progress,
            &format!(
                "boot probe: streaming…/resident… (polls {stream_polls}/{resident_polls}, longest {:.1} ms)",
                longest_poll * 1000.0
            ),
        );
        yield_to_event_loop().await;
    };
    (report, seconds_now() - boot_start, longest_poll)
}

/// One progress line, when the element exists.
#[cfg(target_arch = "wasm32")]
fn note_progress(progress: &web_sys::Element, text: &str) {
    progress.set_text_content(Some(text));
}

/// A zeroed report for a failed drive; the verdict line reports the failure
/// cause already written to the progress line.
#[cfg(target_arch = "wasm32")]
fn placeholder_report() -> (ResidentBootReport, f64, f64) {
    (
        ResidentBootReport {
            high_collections: 0,
            bounds_min: [0; 3],
            bounds_max: [0; 3],
            resident_plans: 0,
            retained_counts: (0, 0),
        },
        0.0,
        0.0,
    )
}

/// The per-archive resident spans, by mount path.
#[cfg(target_arch = "wasm32")]
type Spans = std::collections::BTreeMap<&'static str, std::sync::Arc<ResidentSpans>>;

#[cfg(target_arch = "wasm32")]
use skate2_formats::resident_spans::ResidentSpans;
#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

/// The page's high-resolution clock, in seconds; `Date` if `performance`
/// is absent.
#[cfg(target_arch = "wasm32")]
fn seconds_now() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or_else(
            || js_sys::Date::now() / 1000.0,
            |performance| performance.now() / 1000.0,
        )
}

/// Yields to the browser's event loop with a `setTimeout 0` promise, so
/// long cooperative bursts keep the tab responsive. `requestAnimationFrame`
/// never ticks in background or headless tabs.
#[cfg(target_arch = "wasm32")]
async fn yield_to_event_loop() {
    let promise = js_sys::Promise::new(
        &mut |resolve: js_sys::Function, _reject: js_sys::Function| {
            let window = web_sys::window().expect("a window");
            window.set_timeout_with_callback(&resolve).ok();
        },
    );
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// A byte count as `KiB`/`MiB`, for the panel.
#[cfg(target_arch = "wasm32")]
fn format_mib(bytes: u64) -> String {
    let kib = bytes / 1024;
    if kib >= 1024 {
        format!("{} MiB", kib / 1024)
    } else {
        format!("{kib} KiB")
    }
}
