//! The bytes-in audio system loader (`docs/WEB_PORT_M6_PLAN.md`, phase 1b).
//!
//! Every input the pc host reads from `data\audio` and `data\db\skater.vlt`
//! is bytes here, so the same assembly builds the system on any host — the
//! browser fetches its bytes over HTTP. The archive and entry name tables
//! are the pc host's, moved unchanged; the loose grain files are a recorded
//! list because a byte source cannot enumerate a directory. The vault stays
//! behind [`crate::vault::AttribSource`]: the app implements it over the
//! port's `AttribDb`, so inheritance and layout defaults follow the native
//! `Instance::GetAttributePointer`.

use crate::{
    player::skateboard::GrainFiles,
    system::{AemsFiles, AudioSystem, FeSound},
    vault::{AttribSource, Vault},
};
use skate2_formats::{
    big::{BigArchive, BigEntry, BigEntryKey},
    mount::MountedBig,
};
use std::{collections::HashMap, sync::Arc};

use crate::grain_player::GrainData;

/// The splicer banks `LoadSkateboardData` 0x824ee4b8 (`splicerskateboard.big`) and
/// `LoadCollisionData` 0x824ee348 (`splicercollision.big`) load, in their order, with each bank's
/// `SPLICE_TYPE`.
pub const SKATEBOARD_BANKS: [(&str, usize); 20] = [
    ("CellPhone_Rings.bnk", 27),
    ("Foot_Scrapes.bnk", 17),
    ("L4_Lands_Ollies.bnk", 20),
    ("sk82_cloth.bnk", 23),
    ("Sk82_Whsh_Bys.bnk", 21),
    ("sk8_foley_rattle.bnk", 24),
    ("SK8_Foot_Catch.bnk", 16),
    ("Sk8_Lands_Hrd_Cnc.bnk", 8),
    ("Sk8_Lands_Hrd_Wd.bnk", 9),
    ("Sk8_Lands_Sft_Cnc.bnk", 10),
    ("Sk8_Lands_Sft_Wd.bnk", 11),
    ("SK8_Ollie_Scrapes.bnk", 14),
    ("SK8_Ollies_Cnc.bnk", 12),
    ("SK8_Ollies_Wood.bnk", 13),
    ("SKATE_foot_pushes.bnk", 0),
    ("Splice_dirtfeet.bnk", 3),
    ("Splice_grassfeet.bnk", 4),
    ("Splice_Hands.bnk", 19),
    ("Splice_Smoothconc.bnk", 1),
    ("Splice_woodfoot.bnk", 2),
];

/// `LoadCollisionData` 0x824ee348's banks, in `data\audio\splicercollision.big`.
pub const COLLISION_BANKS: [(&str, usize); 2] =
    [("Skate_Collisions.bnk", 5), ("Skate_Metal.bnk", 6)];

/// `LoadCsisEventSystem` 0x824edc20's interfaces.
pub const AEMS_INTERFACES: [(&str, &str); 8] = [
    ("skateboard.big", "SK8_AEMS_skateboard.csi"),
    ("pedestrian.big", "SK8_AEMS_Foley.csi"),
    ("emitters2.big", "Sk8_Emitters_Project.csi"),
    ("rolling.big", "SK8_AEMS_rolling.csi"),
    ("vehicle.big", "AEMS_TRAFFIC.csi"),
    ("crowds.big", "SK8_AEMS_Crowds.csi"),
    ("dynamicobject.big", "Sk8_AEMS_MoveableObjects.csi"),
    ("moments.big", "Sk8_moments.csi"),
];

/// The AEMS banks in `AudioAssetManager::SyncLoad` order: `Initialize`, `LoadInGameData`
/// (`LoadSkateboardData` 0x824ee4b8, `LoadFoleyData` 0x824f0bf0), then the
/// `SFXObj_SkateBoard` constructor's patch banks.
pub const AEMS_BANKS: [(&str, &str); 20] = [
    ("emitters2.big", "emitter_utility.abk"),
    ("emitters2.big", "Main_Ambience_Crossfade.abk"),
    ("skateboard.big", "board_scrapes.abk"),
    ("skateboard.big", "Brd_Squeaks.abk"),
    ("skateboard.big", "FOOT_DRAG.abk"),
    ("skateboard.big", "GRINDS.abk"),
    ("skateboard.big", "Rolling_Rattles.abk"),
    ("skateboard.big", "Seams_Bank.abk"),
    ("skateboard.big", "sense_of_speed.abk"),
    ("skateboard.big", "Sk8_Air_Flip_Tricks.abk"),
    ("skateboard.big", "Treatments.abk"),
    ("skateboard.big", "WHEEL_SKID_BANK.abk"),
    ("pedestrian.big", "Bodyslide.abk"),
    ("pedestrian.big", "Foley_Cloth.abk"),
    ("pedestrian.big", "fstep_livingworld.abk"),
    ("pedestrian.big", "fstep_skateshoe1_sm.abk"),
    ("rolling.big", "PatchBank_Objects.abk"),
    ("rolling.big", "PatchBank_RocksBounce.abk"),
    ("rolling.big", "PatchBank_Rolling_Surfaces.abk"),
    ("rolling.big", "PatchBank_SpiderCracks.abk"),
];

/// The banks `SFXObj_Dynamic`'s constructor 0x82533670 loads from `data\audio\dynamicobject.big`.
pub const DMO_BANKS: [(&str, &str); 4] = [
    ("dynamicobject.big", "DMO_Starts.abk"),
    ("dynamicobject.big", "Dynamics_Objects_Roll.abk"),
    ("dynamicobject.big", "Dynamics_Objects_Slide_A.abk"),
    ("dynamicobject.big", "Dynamics_Objects_Slide_B.abk"),
];

/// `AudioSystem::LoadMomentsData` 0x824f0e50's banks (`SecondaryLoad`): the modules of
/// `Sk8_moments.csi`'s `fade_to_white`, `killedit_*` and `ownedit_logo`.
pub const MOMENTS_BANKS: [(&str, &str); 3] = [
    ("moments.big", "fade_to_white.abk"),
    ("moments.big", "killed_it.abk"),
    ("moments.big", "owned_it.abk"),
];

/// `AudioSystem::LoadVehiclesData` 0x824fd868's banks in its `AsyncLoadAt` order: the engines
/// (`sEngineNames`), `Traffic_Horn.abk` and `Traffic_Skid.abk` of `vehicle.big`
/// (`mpEnginesBigFileHdr`), then `car_alarms.abk` of `emitters2.big` (`mpEmittersBigFileHdr`).
/// `SecondaryLoad` asks for them and `Update` loads them once no crowd banks are loaded.
pub const VEHICLE_BANKS: [(&str, &str); 11] = [
    ("vehicle.big", "C00_heavy01.abk"),
    ("vehicle.big", "C01_family01.abk"),
    ("vehicle.big", "C03_sports01.abk"),
    ("vehicle.big", "C04_taxi01.abk"),
    ("vehicle.big", "C05_truck01.abk"),
    ("vehicle.big", "C06_sports02.abk"),
    ("vehicle.big", "C07_family02.abk"),
    ("vehicle.big", "C08_family03.abk"),
    ("vehicle.big", "Traffic_Horn.abk"),
    ("vehicle.big", "Traffic_Skid.abk"),
    ("emitters2.big", "car_alarms.abk"),
];

/// `Sk8EmitterSystem::LoadGameFiles` 0x824e9e00's data files, in its order. The loader pushes
/// each as `data\audio\<name>`.
pub const EMITTER_DATA_FILES: [&str; 5] = [
    "sfx.ems",
    "reverb.ems",
    "music.ems",
    "speakers.ems",
    "crowds.ems",
];

/// The loose grain samples and seek tables of `data\audio` (`AudioAssetManager::SyncLoad`,
/// `FileLoader::SyncLoad`), in their on-disk letter case — native path matching ignores case
/// (`host_path`), but the web's byte sources match exactly. The loader keys them lower-case.
/// Recorded once from the NTSC dump (2026-10-23): a byte source cannot enumerate a directory,
/// and the wheels load exactly these.
pub const LOOSE_GRAIN_FILES: [&str; 4] = [
    "Whls_spins_Jump_1.sek",
    "Whls_spins_Jump_1.snr",
    "Whls_spins_Man_1.sek",
    "Whls_spins_Man_1.snr",
];

/// `Attrib::Gen::fe`'s class.
const FE_CLASS: u64 = 0x5831_CB95_F3E9_0598;
const HOM_SPLICE: u64 = 0x845A_1005_2522_A2FB;
const LOGO_TYPE: u64 = 0xA408_0FB6_5880_E3C2;
const FLANGE: u64 = 0xBF45_D439_FAC7_1A2E;
const HOM_EXPIRE_TIME: u64 = 0x9DAD_777A_FEA4_E73C;

/// Every byte input of [`load`], in the shape the pc host reads them.
pub struct AudioInputs {
    /// `data\audio\splicerfe.big`'s `Sk82_FE.bnk`.
    pub fe_bank: Arc<[u8]>,
    /// `data\audio\splicerfe.big`'s `sk8_menu.bnk`.
    pub menu_bank: Arc<[u8]>,
    /// [`SKATEBOARD_BANKS`] as (name, slot, bytes) from `splicerskateboard.big`.
    pub skateboard_banks: Vec<(String, usize, Arc<[u8]>)>,
    /// [`COLLISION_BANKS`] as (name, slot, bytes) from `splicercollision.big`.
    pub collision_banks: Vec<(String, usize, Arc<[u8]>)>,
    /// `splicercollision.big`'s `main_objects::HOM_BANK_NAMES` banks, in order.
    pub hom_banks: Vec<Arc<[u8]>>,
    /// `data\audio\MixMapSK8.mxb`.
    pub mix_map: Vec<u8>,
    /// `data\audio\grains.big`'s `.grain` files plus [`LOOSE_GRAIN_FILES`], by lower-case name.
    pub grains: GrainFiles,
    /// The AEMS interfaces, banks and emitter data files.
    pub aems: AemsFiles,
    /// The lazy stream reader (speech projects, contexts, posts, `.mpf`); the host supplies its
    /// own resident or ranged implementation.
    pub stream_files: Arc<dyn crate::rwac::stream_pool::StreamFiles>,
}

/// The front-end sounds of `skater.vlt`'s `fe` collections and their `FE_SOUND_<NAME>` globals,
/// read through the same `Attrib::Instance` surface the game reads (`layout_word`'s words 0-3,
/// the typed attributes), with `AttribDb`'s big-endian scalar and first-byte bool semantics.
fn fe_sounds(attribs: &dyn AttribSource) -> (HashMap<u64, FeSound>, HashMap<String, u64>) {
    let mut sounds = HashMap::new();
    let mut globals = HashMap::new();
    for collection in attribs.leaf_collections(FE_CLASS) {
        let Some(name) = attribs.layout_text(FE_CLASS, collection, 0) else {
            continue;
        };
        let be32 = |key: u64, index: u32| -> Option<u32> {
            let bytes = attribs.attribute(FE_CLASS, collection, key, index)?;
            (bytes.len() >= 4).then(|| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        };
        let sound = FeSound {
            volume: f32::from_bits(attribs.layout_word(FE_CLASS, collection, 1)),
            menu_splice: attribs.layout_word(FE_CLASS, collection, 2).cast_signed(),
            fe_splice: attribs.layout_word(FE_CLASS, collection, 3).cast_signed(),
            hom_splice: be32(HOM_SPLICE, 0).unwrap_or(0),
            logo_type: be32(LOGO_TYPE, 0).map_or(0, u32::cast_signed),
            flange: attribs
                .attribute(FE_CLASS, collection, FLANGE, 0)
                .is_some_and(|bytes| bytes.first().is_some_and(|byte| *byte != 0)),
            hom_expire_time: be32(HOM_EXPIRE_TIME, 0).map_or(0, u32::cast_signed),
            name: name.clone(),
        };
        globals.insert(
            format!("FE_SOUND_{}", name.to_uppercase()),
            skate2_formats::attrib::string_hash64(&name.to_lowercase()),
        );
        sounds.insert(collection, sound);
    }
    (sounds, globals)
}

/// `AudioSystem` from [`AudioInputs`] and the vault's `AttribSource`: the HOM banks and stream
/// reader set, then `EnterWorld` over the banks, the mix map, grains and AEMS files, exactly as
/// the pc host assembles it (`game_audio.rs`'s `load_system`). The seeds are the native clock
/// draws — which read the clock, so this function traps on wasm32: the browser host calls
/// [`load_with_seeds`].
///
/// # Errors
/// The failing bank or file's stage, as `EnterWorld` reports it.
pub fn load(
    inputs: AudioInputs,
    attribs: Box<dyn AttribSource>,
) -> Result<(AudioSystem, HashMap<String, u64>), String> {
    load_with_seeds(inputs, attribs, crate::system::SystemSeeds::from_clock())
}

/// [`load`] with explicit seeds (the reproducible-host route of phase 1a).
///
/// # Errors
/// The failing bank or file's stage, as `EnterWorld` reports it.
pub fn load_with_seeds(
    inputs: AudioInputs,
    attribs: Box<dyn AttribSource>,
    seeds: crate::system::SystemSeeds,
) -> Result<(AudioSystem, HashMap<String, u64>), String> {
    let (sounds, globals) = fe_sounds(attribs.as_ref());
    let mut system = AudioSystem::new_with_seeds(seeds);
    system.set_hom_banks(inputs.hom_banks);
    system.set_stream_files(inputs.stream_files);
    let mut world_banks = inputs.skateboard_banks;
    world_banks.extend(inputs.collision_banks);
    system.enter_world(
        Vault::new(Some(attribs)),
        inputs.fe_bank,
        inputs.menu_bank,
        world_banks,
        inputs.mix_map,
        sounds,
        inputs.grains,
        inputs.aems,
    )?;
    Ok((system, globals))
}

/// One loose grain file of [`LOOSE_GRAIN_FILES`] as loader input: (lower-case name, bytes).
#[must_use]
pub fn loose_grain(name: &str, bytes: Vec<u8>) -> (String, GrainData) {
    (
        name.to_lowercase(),
        GrainData {
            bytes: Arc::from(bytes),
        },
    )
}

/// Where the loader's `data\audio` files come from: the pc host mounts the
/// archives from disk, the browser over the spans it prefetched
/// ([`wanted_entries`]). Both feed the same [`read_inputs`].
pub trait AudioFiles {
    /// `data\audio\<name>`, mounted.
    ///
    /// # Errors
    /// The archive cannot be opened.
    fn archive(&self, name: &str) -> Result<MountedBig, String>;
    /// `data\audio\<name>`'s bytes; `None` when the file is absent.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    fn loose(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
}

/// Every `data\audio` archive [`read_inputs`] mounts.
pub const ARCHIVES: [&str; 13] = [
    "splicerfe.big",
    "splicerskateboard.big",
    "splicercollision.big",
    "grains.big",
    "emitters.big",
    "emitters2.big",
    "skateboard.big",
    "pedestrian.big",
    "rolling.big",
    "vehicle.big",
    "crowds.big",
    "dynamicobject.big",
    "moments.big",
];

/// Every loose `data\audio` file [`read_inputs`] reads: the mix map and the
/// emitter data files are required, [`LOOSE_GRAIN_FILES`] optional.
pub fn loose_files() -> impl Iterator<Item = &'static str> {
    std::iter::once(MIX_MAP)
        .chain(EMITTER_DATA_FILES)
        .chain(LOOSE_GRAIN_FILES)
}

const MIX_MAP: &str = "MixMapSK8.mxb";

/// The front-end banks of `splicerfe.big`.
const FE_BANKS: [&str; 2] = ["Sk82_FE.bnk", "sk8_menu.bnk"];

/// The entries of `data\audio\<name>` that [`read_inputs`] reads, so a ranged
/// host can make exactly those resident first. Archives read whole
/// (`grains.big`, `emitters.big`) answer every named entry.
#[must_use]
pub fn wanted_entries<'a>(name: &str, archive: &'a BigArchive) -> Vec<&'a BigEntry> {
    let named: Vec<&str> = match name {
        "grains.big" | "emitters.big" => {
            return archive
                .entries
                .iter()
                .filter(|entry| matches!(entry.key, BigEntryKey::Name(_)))
                .collect();
        }
        "splicerfe.big" => FE_BANKS.to_vec(),
        "splicerskateboard.big" => SKATEBOARD_BANKS.iter().map(|(bank, _)| *bank).collect(),
        "splicercollision.big" => COLLISION_BANKS
            .iter()
            .map(|(bank, _)| *bank)
            .chain(crate::main_objects::HOM_BANK_NAMES)
            .collect(),
        _ => AEMS_INTERFACES
            .iter()
            .chain(&AEMS_BANKS)
            .chain(&DMO_BANKS)
            .chain(&MOMENTS_BANKS)
            .chain(&VEHICLE_BANKS)
            .filter(|(big, _)| *big == name)
            .map(|(_, file)| *file)
            .collect(),
    };
    named
        .into_iter()
        .filter_map(|file| archive.find_path(file))
        .collect()
}

/// Every byte input of [`load`] from `files`, with the caller's stream reader:
/// the pc host's assembly, moved here so every host runs the same one.
///
/// # Errors
/// The failing archive, entry or file.
pub fn read_inputs(
    files: &dyn AudioFiles,
    stream_files: Arc<dyn crate::rwac::stream_pool::StreamFiles>,
) -> Result<AudioInputs, String> {
    let fe = files.archive("splicerfe.big")?;
    let [fe_bank, menu_bank] = FE_BANKS.map(|bank| {
        fe.read_path(bank)
            .map(Arc::from)
            .map_err(|e| format!("{bank}: {e}"))
    });
    Ok(AudioInputs {
        fe_bank: fe_bank?,
        menu_bank: menu_bank?,
        skateboard_banks: splicer_banks(files, "splicerskateboard.big", &SKATEBOARD_BANKS)?,
        collision_banks: splicer_banks(files, "splicercollision.big", &COLLISION_BANKS)?,
        hom_banks: hom_banks(files)?,
        mix_map: files
            .loose(MIX_MAP)?
            .ok_or_else(|| format!("{MIX_MAP}: absent"))?,
        grains: grain_files(files)?,
        aems: aems_files(files)?,
        stream_files,
    })
}

/// One splicer bank as [`AudioInputs`] carries it: (name, `SPLICE_TYPE` slot, bytes).
type SplicerBank = (String, usize, Arc<[u8]>);

/// The banks of one `data\audio` splicer archive.
fn splicer_banks(
    files: &dyn AudioFiles,
    archive: &str,
    banks: &[(&str, usize)],
) -> Result<Vec<SplicerBank>, String> {
    let big = files.archive(archive)?;
    banks
        .iter()
        .map(|&(name, slot)| {
            big.read_path(name)
                .map(|bytes| (name.to_owned(), slot, Arc::from(bytes)))
                .map_err(|e| format!("{name}: {e}"))
        })
        .collect()
}

/// `GetHOMBank` 0x824e6a08's banks (`sHOMBankNames`), in their order.
fn hom_banks(files: &dyn AudioFiles) -> Result<Vec<Arc<[u8]>>, String> {
    let big = files.archive("splicercollision.big")?;
    crate::main_objects::HOM_BANK_NAMES
        .iter()
        .map(|&name| {
            big.read_path(name)
                .map(Arc::from)
                .map_err(|e| format!("{name}: {e}"))
        })
        .collect()
}

/// `data\audio\grains.big`'s `.grain` files, which the skateboards' `LoadDataForSurface` loads by
/// their `GrainFile` name, and the loose `data\audio` samples and seek tables (`.snr`, `.sek`)
/// the wheels load by name (`AudioAssetManager::SyncLoad`, `FileLoader::SyncLoad`) — the recorded
/// list, because the browser's byte source cannot enumerate a directory.
fn grain_files(files: &dyn AudioFiles) -> Result<GrainFiles, String> {
    let big = files.archive("grains.big")?;
    let mut grains = GrainFiles::new();
    for entry in &big.archive().entries {
        let BigEntryKey::Name(name) = &entry.key else {
            continue;
        };
        let bytes = big.read_entry(entry).map_err(|e| format!("{name}: {e}"))?;
        grains.insert(
            name.to_lowercase(),
            GrainData {
                bytes: Arc::from(bytes),
            },
        );
    }
    for name in LOOSE_GRAIN_FILES {
        // The old directory walk skipped a missing file; keep that: a dump
        // without these loses two wheel sounds, not all audio.
        let Some(bytes) = files.loose(name)? else {
            continue;
        };
        let (name, grain) = loose_grain(name, bytes);
        grains.insert(name, grain);
    }
    Ok(grains)
}

fn big_file(files: &dyn AudioFiles, big: &str, name: &str) -> Result<Vec<u8>, String> {
    let archive = files.archive(big)?;
    let entry = archive
        .archive()
        .find_name(name)
        .ok_or_else(|| format!("{big}: no {name}"))?;
    archive
        .read_entry(entry)
        .map_err(|e| format!("{name}: {e}"))
}

fn aems_files(files: &dyn AudioFiles) -> Result<AemsFiles, String> {
    let mut aems = AemsFiles::default();
    for name in EMITTER_DATA_FILES {
        let bytes = files
            .loose(name)?
            .ok_or_else(|| format!("{name}: absent"))?;
        aems.emitters
            .data_files
            .push((format!("data\\audio\\{name}"), bytes));
    }
    // The banks the emitter system's swap slots load from `data\audio\emitters.big`.
    let emitters = files.archive("emitters.big")?;
    for entry in &emitters.archive().entries {
        if let BigEntryKey::Name(name) = &entry.key {
            let bytes = emitters
                .read_entry(entry)
                .map_err(|e| format!("{name}: {e}"))?;
            aems.emitters.banks.push((name.clone(), Arc::from(bytes)));
        }
    }
    for (big, name) in AEMS_INTERFACES {
        aems.interfaces
            .push((name.to_owned(), big_file(files, big, name)?));
    }
    for (big, name) in AEMS_BANKS
        .iter()
        .chain(&DMO_BANKS)
        .chain(&MOMENTS_BANKS)
        .chain(&VEHICLE_BANKS)
    {
        aems.banks
            .push(((*name).to_owned(), Arc::from(big_file(files, big, name)?)));
    }
    Ok(aems)
}
