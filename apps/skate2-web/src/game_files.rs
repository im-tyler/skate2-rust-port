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
use std::collections::BTreeMap;
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

    /// One request for `path`, with `method`.
    async fn request(&self, path: &str, method: &str) -> Result<web_sys::Response, String> {
        let url = format!("{}/{}", self.base, path);
        let init = web_sys::RequestInit::new();
        init.set_method(method);
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
        Ok(self.request(path, "HEAD").await?.ok())
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let response = self.request(path, "GET").await?;
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
    AssetsReport {
        found,
        missing,
        parsed,
    }
}

/// Whether the bytes begin like a BIG table at all, so a data-only `.big`
/// can be told from a table before parsing.
fn table(bytes: &[u8]) -> Option<()> {
    let magic = bytes.get(..4)?;
    (magic.starts_with(b"BIG") || magic == b"Viv4").then_some(())
}
