//! Milestone three (`docs/WEB_PORT.md`): the Vault attribute database parsed
//! in the browser. The `.vlt`/`.bin` pair inside `db.big` is read through the
//! ranged sources of [`crate::game_files`], then interpreted with the real
//! `skate2_formats` vault reader — the same chain core's `AttribDb::load`
//! walks (`crates/skate2-core/src/challenge/attrib_db.rs`), mirrored so the
//! browser shows what the game itself would read.

use crate::game_files::{ByteSource, RemoteArchive};
use skate2_formats::attrib::{
    AttributeDataFile, AttributeExportKind, AttributePointerLocation, AttributePointerValue,
    AttributeVaultMetadata, string_hash64,
};
use std::collections::{BTreeMap, BTreeSet};

/// What the panel shows of one vault pair.
pub(crate) struct VaultSummary {
    /// The packed type names the database export carries.
    pub(crate) types: Vec<String>,
    /// How many class exports the vault holds.
    pub(crate) classes: usize,
    /// How many attribute definitions those classes carry altogether.
    pub(crate) definitions: usize,
    /// The class names the panel verifies by hash, as the native tools pin
    /// them from the executable (`skater.bin`'s string table does not carry
    /// most class names).
    pub(crate) class_names: Vec<String>,
    /// `physics`'s `default` collection's first layout words, as the corpus
    /// test pins them — word one is the game's gravity.
    pub(crate) physics_words: [u32; 4],
    /// The dependency (`.bin`) file names the vault names, read as data.
    pub(crate) dependencies: Vec<String>,
}

/// Reads and parses one `.vlt` entry of an opened remote archive, following
/// its named dependencies in the same archive, and summarises the database
/// the way core's loader does.
///
/// # Errors
///
/// Missing entries, unparsable chunks, or pointer fixups that do not resolve;
/// see the panel's line for the message.
pub(crate) async fn inspect_vault<S: ByteSource + ?Sized>(
    source: &S,
    archive: &RemoteArchive,
    vlt_name: &str,
) -> Result<VaultSummary, String> {
    let vlt = archive
        .read_entry_by_name(source, vlt_name)
        .await
        .map_err(|problem| format!("{vlt_name}: {problem}"))?;
    let metadata =
        AttributeVaultMetadata::parse(&vlt).map_err(|error| format!("{vlt_name}: {error}"))?;
    // The vault names its data files (the `.bin` sibling); read each from the
    // same archive, relative to the vault's own directory.
    let directory = vlt_name
        .rfind(['/', '\\'])
        .map_or("", |end| &vlt_name[..end]);
    let mut buffers: Vec<Vec<u8>> = vec![vlt];
    let mut dependencies = Vec::new();
    for dependency in &metadata.dependencies {
        let path = format!("{directory}\\{}", dependency.name);
        let bytes = archive
            .read_entry_by_name(source, &path)
            .await
            .map_err(|problem| format!("{path}: {problem}"))?;
        dependencies.push(dependency.name.clone());
        buffers.push(bytes);
    }
    let sizes: Vec<usize> = buffers.iter().map(Vec::len).collect();
    let pointers = metadata
        .resolve_pointers(&sizes)
        .map_err(|error| format!("{vlt_name}: {error}"))?;
    let slices: Vec<&[u8]> = buffers.iter().map(AsRef::as_ref).collect();
    let vlt = &buffers[0];
    let class_ids: BTreeSet<u64> = metadata
        .exports
        .iter()
        .filter(|export| export.kind() == AttributeExportKind::Class)
        .map(|export| export.object_id)
        .collect();
    let mut types = Vec::new();
    let mut classes = 0;
    let mut definitions = 0;
    for export in &metadata.exports {
        match export.kind() {
            AttributeExportKind::Database => {
                types = read_type_names(&buffers, &pointers, vlt, export, vlt_name)?;
            }
            AttributeExportKind::Class => {
                classes += 1;
                let parsed = export
                    .class_definitions(&slices, &pointers)
                    .map_err(|error| format!("{vlt_name}: {error}"))?;
                definitions += parsed.len();
            }
            _ => {}
        }
    }
    let class_names = known_class_names(&class_ids);
    let physics_words = physics_layout_words(&buffers, &pointers, &metadata);
    Ok(VaultSummary {
        types,
        classes,
        definitions,
        class_names,
        physics_words,
        dependencies,
    })
}

/// The classes the panel names: hashes the native tools pin from the
/// executable, verified against this vault's class export ids.
fn known_class_names(class_ids: &BTreeSet<u64>) -> Vec<String> {
    ["physics", "fe", "characters_marquee", "skeleton"]
        .into_iter()
        .filter(|name| class_ids.contains(&string_hash64(name)))
        .map(str::to_owned)
        .collect()
}

/// A resolved direct pointer in dependency zero, as core's `direct` closure
/// reads it: which buffer, and where in it.
fn direct_pointer(
    pointers: &BTreeMap<AttributePointerLocation, AttributePointerValue>,
    offset: u32,
) -> Option<(usize, usize)> {
    match pointers.get(&AttributePointerLocation {
        dependency_index: 0,
        offset,
    }) {
        Some(AttributePointerValue::Direct {
            dependency_index,
            offset,
        }) => Some((
            usize::from(*dependency_index),
            usize::try_from(*offset).ok()?,
        )),
        _ => None,
    }
}

/// The database export's type names: `type_count` packed NUL-terminated
/// strings behind the direct pointer at `data_offset + 12`.
fn read_type_names(
    buffers: &[Vec<u8>],
    pointers: &BTreeMap<AttributePointerLocation, AttributePointerValue>,
    vlt: &[u8],
    export: &skate2_formats::attrib::AttributeExport,
    vlt_name: &str,
) -> Result<Vec<String>, String> {
    let data = export
        .database_load_data(vlt)
        .map_err(|error| format!("{vlt_name}: {error}"))?;
    let mut names = Vec::new();
    let Some((dependency, mut at)) = direct_pointer(pointers, export.data_offset + 12) else {
        return Ok(names);
    };
    for _ in 0..data.type_count {
        let Some(len) = buffers
            .get(dependency)
            .and_then(|buffer| buffer.get(at..))
            .and_then(|rest| rest.iter().position(|&byte| byte == 0))
        else {
            break;
        };
        let Some(name) = buffers
            .get(dependency)
            .map(|buffer| String::from_utf8_lossy(&buffer[at..at + len]).into_owned())
        else {
            break;
        };
        names.push(name);
        at += len + 1;
    }
    Ok(names)
}

/// The class names the `.bin`'s string table carries, hashed against the
/// class export ids — kept for the future runtime, which resolves names the
/// game hashed; the panel shows the pinned ones instead.
#[expect(
    dead_code,
    reason = "the runtime name table arrives with milestone four"
)]
fn class_names_from_bin(bin: &[u8], class_ids: &BTreeSet<u64>) -> Vec<String> {
    let Ok(data_file) = AttributeDataFile::parse(bin) else {
        return Vec::new();
    };
    let mut names: Vec<String> = data_file
        .strings
        .iter()
        .filter(|string| class_ids.contains(&string_hash64(&string.value)))
        .map(|string| string.value.clone())
        .collect();
    names.sort();
    names
}

/// `physics`'s `default` collection's first four layout words. A collection
/// export's identity sits in its payload — `collection_load_data` carries
/// the class and collection ids — and the layout pointer at the export's
/// `data_offset + 40` (`CollectionLoadData::mLayout`). The corpus test pins
/// the words: word one is the game's gravity, bits `0xc11c_cccd` (-9.8).
fn physics_layout_words(
    buffers: &[Vec<u8>],
    pointers: &BTreeMap<AttributePointerLocation, AttributePointerValue>,
    metadata: &AttributeVaultMetadata,
) -> [u32; 4] {
    let physics = string_hash64("physics");
    let default = string_hash64("default");
    let Some(vlt) = buffers.first() else {
        return [0; 4];
    };
    let Some(collection) = metadata.exports.iter().find(|export| {
        export.kind() == AttributeExportKind::Collection
            && export
                .collection_load_data(vlt)
                .is_ok_and(|data| data.class_id == physics && data.collection_id == default)
    }) else {
        return [0; 4];
    };
    let Some(slot) = collection.data_offset.checked_add(40) else {
        return [0; 4];
    };
    let Some((dependency, offset)) = direct_pointer(pointers, slot) else {
        return [0; 4];
    };
    buffers
        .get(dependency)
        .and_then(|buffer| buffer.get(offset..offset.saturating_add(16)))
        .map_or([0; 4], |bytes| {
            [
                u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
                u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            ]
        })
}
