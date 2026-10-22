//! The browser build's game bytes: where the original game's files come from
//! in a page that has no file system (`docs/WEB_PORT.md`, asset pipeline step
//! one). Two sources share one job — read a whole file's bytes by its mount
//! path, like `data/big/render.big`:
//!
//! - [`HttpFiles`] fetches from a base URL. `?http=<base>` on the page arms
//!   it; that is both the verification route (a local server standing in for
//!   the extracted dump) and the shape of the future self-hosted streaming
//!   option, whose server would carry converted packs instead of originals.
//! - [`PickedFiles`] holds the files of a folder the player picked
//!   (`webkitdirectory`), so their own dump never leaves their machine — the
//!   page's origin never serves or stores any original asset.

use skate2_formats::big::BigArchive;
use std::{collections::BTreeMap, ops::Range};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The archive paths the native build mounts (`skate2_formats::mount`), as
/// the page's canonical forward-slash forms.
pub(crate) const MOUNT_PATHS: [&str; 20] = [
    "data/big/render.big",
    "data/big/miscboot.big",
    "data/big/colourcubes.big",
    "data/db/db.big",
    "data/big/scene.big",
    "data/content/worldproxy.big",
    "data/content/worldbam.big",
    "data/content/worldmisc.big",
    "data/content/assettoc_cac.big",
    "data/content/assettoc_lw.big",
    "data/content/assettoc_mqe.big",
    "data/big/miscload.big",
    "data/big/globallocators.big",
    "data/big/anim.big",
    "data/content/missions.big",
    "data/big/aipaths.big",
    "data/content/livingworld.big",
    "data/content/marquee.big",
    "data/big/ropa2.big",
    "data/content/createacharacter.big",
];

/// The smallest self-contained archives whose full tables the panel parses:
/// `Viv4` archives keep their tables in a `.bh` beside the data file, so the
/// `.bh` is read when the `.big` itself is not a table.
const PARSED_PATHS: [&str; 2] = ["data/big/globallocators.big", "data/db/db.big"];

/// One byte source: read a mount path's whole file.
pub(crate) trait ByteSource {
    /// Whether a path exists, without reading it.
    async fn present(&self, path: &str) -> Result<bool, String>;
    /// The whole file's bytes.
    async fn read(&self, path: &str) -> Result<Vec<u8>, String>;
    /// A byte range of a file. The default reads the whole file and slices;
    /// ranged-capable sources override, because a whole-file read of a
    /// multi-gigabyte archive is exactly what streaming exists to avoid.
    async fn read_range(&self, path: &str, range: Range<u64>) -> Result<Vec<u8>, String> {
        let start =
            usize::try_from(range.start).map_err(|_| format!("{path}: range start too large"))?;
        let end = usize::try_from(range.end).map_err(|_| format!("{path}: range end too large"))?;
        let bytes = self.read(path).await?;
        bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| format!("{path}: {start}..{end} is outside the file"))
    }
}

/// Files fetched from a base URL, one resource per mount path.
pub(crate) struct HttpFiles {
    base: String,
}

impl HttpFiles {
    /// The base URL every mount path is fetched under. CORS applies: the
    /// server must allow the page's origin.
    pub(crate) fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// One request for `path`, with `method` and an optional byte range.
    async fn request(
        &self,
        path: &str,
        method: &str,
        range: Option<Range<u64>>,
    ) -> Result<web_sys::Response, String> {
        let url = format!("{}/{}", self.base, path);
        let init = web_sys::RequestInit::new();
        init.set_method(method);
        if let Some(range) = range {
            let headers = web_sys::Headers::new().map_err(|error| format!("{path}: {error:?}"))?;
            // The HTTP range is inclusive at its end; the logical range is not.
            headers
                .set(
                    "Range",
                    &format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
                )
                .map_err(|error| format!("{path}: {error:?}"))?;
            init.set_headers(headers.as_ref());
        }
        let request = web_sys::Request::new_with_str_and_init(&url, &init)
            .map_err(|error| format!("{path}: {error:?}"))?;
        let promise = web_sys::window()
            .ok_or("no Window")?
            .fetch_with_request(&request);
        let value = JsFuture::from(promise)
            .await
            .map_err(|error| format!("{path}: {error:?}"))?;
        value
            .dyn_into::<web_sys::Response>()
            .map_err(|_| format!("{path}: fetch returned no Response"))
    }
}

impl ByteSource for HttpFiles {
    async fn present(&self, path: &str) -> Result<bool, String> {
        Ok(self.request(path, "HEAD", None).await?.ok())
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let response = self.request(path, "GET", None).await?;
        if !response.ok() {
            return Err(format!("{path}: HTTP {}", response.status()));
        }
        array_buffer_bytes(
            path,
            response
                .array_buffer()
                .map_err(|error| format!("{path}: {error:?}"))?,
        )
        .await
    }

    async fn read_range(&self, path: &str, range: Range<u64>) -> Result<Vec<u8>, String> {
        let response = self.request(path, "GET", Some(range.clone())).await?;
        let bytes = array_buffer_bytes(
            path,
            response
                .array_buffer()
                .map_err(|error| format!("{path}: {error:?}"))?,
        )
        .await?;
        match response.status() {
            // 206 Partial Content: exactly the requested range.
            206 => Ok(bytes),
            // A server that ignores Range returns 200 with the whole file;
            // slice it rather than fail, the way a cache would.
            200 if response
                .headers()
                .get("Content-Range")
                .ok()
                .flatten()
                .is_none() =>
            {
                let start = usize::try_from(range.start).map_err(|_| "range start too large")?;
                let end = usize::try_from(range.end).map_err(|_| "range end too large")?;
                bytes
                    .get(start..end)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| format!("{path}: {start}..{end} is outside the file"))
            }
            status => Err(format!("{path}: HTTP {status} for a ranged read")),
        }
    }
}

/// The files of one picked folder, by their mount paths.
pub(crate) struct PickedFiles {
    files: BTreeMap<String, web_sys::Blob>,
}

impl PickedFiles {
    /// Collects the chosen folder's files from the picker input. The picker
    /// reports each file under the picked folder's name, so that first hop is
    /// dropped: picking the dump root makes `data/...` the mount paths.
    pub(crate) fn from_input(input: &web_sys::HtmlInputElement) -> Result<Self, String> {
        let list = input.files().ok_or("the picker reported no files")?;
        let mut files = BTreeMap::new();
        for index in 0..list.length() {
            let Some(file) = list.item(index) else {
                continue;
            };
            let relative =
                js_sys::Reflect::get(file.as_ref(), &JsValue::from_str("webkitRelativePath"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .unwrap_or_default();
            // Backslashes (a Windows dump picked into another browser) and
            // the leading picked-folder hop both normalise away here.
            let path = relative
                .replace('\\', "/")
                .split_once('/')
                .map_or_else(|| relative.clone(), |(_, rest)| rest.to_owned());
            if !path.is_empty() {
                files.insert(
                    path,
                    file.dyn_into::<web_sys::Blob>().map_err(|_| "not a file")?,
                );
            }
        }
        if files.is_empty() {
            return Err("the picked folder held no files".to_owned());
        }
        Ok(Self { files })
    }

    /// Whether the pick carried nothing (it always carries something after
    /// [`Self::from_input`] succeeds; the pair keeps clippy's `len` lint).
    #[expect(
        dead_code,
        reason = "the empty pick is an error before this could report it"
    )]
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// How many files the pick carried, for the status line.
    pub(crate) fn len(&self) -> usize {
        self.files.len()
    }
}

impl ByteSource for PickedFiles {
    async fn present(&self, path: &str) -> Result<bool, String> {
        // The in-memory map needs no I/O; the ready future keeps the trait's
        // async shape (clippy's unused_async cannot be silenced on trait
        // impl bodies in this version, so this awaits honestly instead).
        Ok(std::future::ready(self.files.contains_key(path)).await)
    }
    async fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let file = self
            .files
            .get(path)
            .ok_or_else(|| format!("{path}: not in the picked folder"))?;
        array_buffer_bytes(path, file.array_buffer()).await
    }

    // `Blob::slice` reads only the wanted span; its `f64` offsets cover every
    // archive size a browser can hold, and the guard keeps the cast honest.
    #[expect(
        clippy::cast_precision_loss,
        reason = "ranges past f64 precision (8 EiB) cannot name a browser file"
    )]
    async fn read_range(&self, path: &str, range: Range<u64>) -> Result<Vec<u8>, String> {
        let file = self
            .files
            .get(path)
            .ok_or_else(|| format!("{path}: not in the picked folder"))?;
        if range.end > 1u64 << 53 {
            return Err(format!("{path}: range end past f64 precision"));
        }
        let slice = file
            .slice_with_f64_and_f64(range.start as f64, range.end as f64)
            .map_err(|error| format!("{path}: {error:?}"))?;
        array_buffer_bytes(path, slice.array_buffer()).await
    }
}

/// An `arrayBuffer` promise's bytes, without `unsafe` — `Response` and `Blob`
/// both expose the one reader the sources need.
async fn array_buffer_bytes(path: &str, promise: js_sys::Promise) -> Result<Vec<u8>, String> {
    let value = JsFuture::from(promise)
        .await
        .map_err(|error| format!("{path}: {error:?}"))?;
    let buffer = value
        .dyn_into::<js_sys::ArrayBuffer>()
        .map_err(|_| format!("{path}: read returned no buffer"))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// The presence of every mounted archive, and the parsed tables of a few of
/// them, as the assets panel's lines.
pub(crate) struct AssetsReport {
    /// How many of [`MOUNT_PATHS`] the source has.
    pub(crate) found: usize,
    /// The mount paths that were absent or unreachable.
    pub(crate) missing: Vec<String>,
    /// The archives whose tables were parsed.
    pub(crate) parsed: Vec<ParsedArchive>,
    /// Step two's proof: one whole entry read out of `db.big` through ranged
    /// requests, RefPack-expanded if it was stored compressed.
    pub(crate) entry_read: String,
    /// Step three's proof: the Vault pair's database summary.
    pub(crate) vault_line: String,
}

/// One parsed archive: its path and a sample of its entries.
pub(crate) struct ParsedArchive {
    pub(crate) path: &'static str,
    pub(crate) entries: usize,
    pub(crate) sample: Vec<String>,
}

/// Checks the dump the source carries: presence of all twenty mounted
/// archives, then the full tables of the two smallest self-contained ones
/// through the real formats code — the `.bh` when the `.big` is not itself a
/// table, as `Viv4` archives store it.
pub(crate) async fn inspect<S: ByteSource + ?Sized>(source: &S) -> AssetsReport {
    let mut found = 0;
    let mut missing = Vec::new();
    for path in MOUNT_PATHS {
        // A transport error counts as absent; the panel lists the path.
        if source.present(path).await.unwrap_or(false) {
            found += 1;
        } else {
            missing.push(path.to_owned());
        }
    }
    let mut parsed = Vec::new();
    for path in PARSED_PATHS {
        let header_path = || path.replace(".big", ".bh");
        let bytes = match source.read(path).await {
            Ok(bytes) if table(&bytes).is_some() => Ok(bytes),
            Ok(bytes) => match source.read(&header_path()).await {
                // The `.big` is data; its table sits in the `.bh`.
                Ok(header_bytes) if table(&header_bytes).is_some() => Ok(header_bytes),
                _ => Ok(bytes),
            },
            Err(error) => Err(error),
        };
        let report = |entries: usize, sample: Vec<String>| ParsedArchive {
            path,
            entries,
            sample,
        };
        parsed.push(match bytes.map_err(|error| vec![error]) {
            Ok(bytes) => match BigArchive::parse(&bytes) {
                Ok(archive) => report(
                    archive.entries.len(),
                    archive
                        .entries
                        .iter()
                        .take(4)
                        .map(|entry| match &entry.key {
                            skate2_formats::big::BigEntryKey::Name(name) => name.clone(),
                            skate2_formats::big::BigEntryKey::Hash(hash) => format!("{hash:#010x}"),
                        })
                        .collect(),
                ),
                Err(error) => report(0, vec![format!("parse failed: {error}")]),
            },
            Err(sample) => report(0, sample),
        });
    }
    // Step two: one whole entry out of the Vault database, over ranges only.
    // Step three: that entry's Vault parsed — types, classes, definitions.
    let (entry_read, vault_line) = db_proof(source).await;
    AssetsReport {
        found,
        missing,
        parsed,
        entry_read,
        vault_line,
    }
}

/// Steps two and three's proofs from one opened `db.big`: its first named
/// entry read whole over ranges, and the Vault pair's parsed summary.
async fn db_proof<S: ByteSource + ?Sized>(source: &S) -> (String, String) {
    let Ok(db) = RemoteArchive::open(source, "data/db/db.big").await else {
        return (
            "db.big remote open failed".to_owned(),
            "db.big remote open failed".to_owned(),
        );
    };
    let named = db
        .archive
        .entries
        .iter()
        .find(|entry| matches!(&entry.key, skate2_formats::big::BigEntryKey::Name(_)));
    let entry_read = match named {
        Some(entry) => {
            let name = match &entry.key {
                skate2_formats::big::BigEntryKey::Name(name) => name.clone(),
                skate2_formats::big::BigEntryKey::Hash(hash) => format!("{hash:#010x}"),
            };
            match db.read_entry_by_name(source, &name).await {
                Ok(bytes) => format!(
                    "{name}: {} stored → {} bytes{}",
                    entry.stored_size,
                    bytes.len(),
                    if entry.stored_size < entry.unpacked_size {
                        ", RefPack"
                    } else {
                        ""
                    }
                ),
                Err(problem) => format!("{name}: {problem}"),
            }
        }
        None => "db.big holds no named entry to read".to_owned(),
    };
    let vault_line = match crate::vault::inspect_vault(source, &db, "data\\db\\skater.vlt").await {
        Ok(summary) => vault_line(&summary),
        Err(problem) => format!("skater.vlt: {problem}"),
    };
    (entry_read, vault_line)
}

/// The panel's Vault line from a parsed summary.
fn vault_line(summary: &crate::vault::VaultSummary) -> String {
    let gravity = f32::from_bits(summary.physics_words[1]);
    let names = summary
        .class_names
        .iter()
        .filter(|name| matches!(name.as_str(), "physics" | "fe" | "characters_marquee"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "{} types, {} classes ({} named, e.g. {}), {} definitions; physics.default gravity {} ({:#010x}); needs {}",
        summary.types.len(),
        summary.classes,
        summary.class_names.len(),
        names,
        summary.definitions,
        gravity,
        summary.physics_words[1],
        summary.dependencies.join(", ")
    )
}

/// Whether the bytes begin like a BIG table at all, so a data-only `.big`
/// can be told from a table before parsing.
fn table(bytes: &[u8]) -> Option<()> {
    let magic = bytes.get(..4)?;
    (magic.starts_with(b"BIG") || magic == b"Viv4").then_some(())
}

/// Whether stored bytes carry the `RefPack` signature, as the native mount
/// probes them (mirroring the private check in the `skate2_formats` mount).
fn is_refpack(bytes: &[u8]) -> bool {
    bytes
        .get(..2)
        .is_some_and(|prefix| u16::from_be_bytes([prefix[0], prefix[1]]) & 0x3eff == 0x10fb)
}

/// A big-endian `u32` at `offset`, as the named header's sizes are stored.
fn header_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
}

/// One opened remote archive: its parsed table and the data file the entry
/// offsets refer to (the `.big`, also when a `.bh` sibling held the table).
pub(crate) struct RemoteArchive {
    /// The parsed entry table.
    pub(crate) archive: BigArchive,
    /// The path entry offsets refer into.
    pub(crate) data_path: String,
}

impl RemoteArchive {
    /// Opens one archive's table with ranged reads: sixteen bytes decide
    /// whether the `.big` itself begins a table (and how large it is) or
    /// whether the table sits in its `.bh` sibling.
    pub(crate) async fn open<S: ByteSource + ?Sized>(
        source: &S,
        path: &str,
    ) -> Result<Self, String> {
        let header = source.read_range(path, 0..16).await?;
        if table(&header).is_some() {
            // `count` sits at 8 and `header_size` at 12 in the named header.
            let header_size =
                u64::from(header_u32(&header, 12).ok_or("header too short for its size")?);
            let table_bytes = source.read_range(path, 0..header_size).await?;
            let archive =
                BigArchive::parse(&table_bytes).map_err(|error| format!("{path}: {error}"))?;
            return Ok(Self {
                archive,
                data_path: path.to_owned(),
            });
        }
        let header_path = path.replace(".big", ".bh");
        let table_bytes = source.read(&header_path).await?;
        let archive =
            BigArchive::parse(&table_bytes).map_err(|error| format!("{header_path}: {error}"))?;
        Ok(Self {
            archive,
            data_path: path.to_owned(),
        })
    }

    /// One entry's bytes by its stored name, read over ranges.
    pub(crate) async fn read_entry_by_name<S: ByteSource + ?Sized>(
        &self,
        source: &S,
        name: &str,
    ) -> Result<Vec<u8>, String> {
        let entry = self
            .archive
            .find_path(name)
            .or_else(|| self.archive.find_path(&name.replace('\\', "/")))
            .ok_or_else(|| "no such entry".to_owned())?;
        read_remote_entry(source, &self.data_path, entry).await
    }
}

/// One entry's unpacked bytes from a ranged source, with the native mount's
/// semantics: read the stored span out of `data_path`, RefPack-expand when
/// the signature says so, and validate hashed entries' unpacked size.
pub(crate) async fn read_remote_entry<S: ByteSource + ?Sized>(
    source: &S,
    data_path: &str,
    entry: &skate2_formats::big::BigEntry,
) -> Result<Vec<u8>, String> {
    let end = entry
        .offset
        .checked_add(entry.stored_size)
        .ok_or("entry offset arithmetic overflow")?;
    let stored = source.read_range(data_path, entry.offset..end).await?;
    let unpacked = if is_refpack(&stored) {
        skate2_formats::refpack::decompress(&stored).map_err(|error| format!("RefPack: {error}"))?
    } else {
        stored
    };
    if matches!(entry.key, skate2_formats::big::BigEntryKey::Hash(_))
        && u64::try_from(unpacked.len()).ok() != Some(entry.unpacked_size)
    {
        return Err(format!(
            "entry {}: unpacked {} but the header says {}",
            entry.header_index,
            unpacked.len(),
            entry.unpacked_size
        ));
    }
    Ok(unpacked)
}
