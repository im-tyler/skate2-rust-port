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
/// (host_path), but the web's byte sources match exactly. The loader keys them lower-case.
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
