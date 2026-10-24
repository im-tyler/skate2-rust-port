//! The renderables `SceneCharacterManager::CreateCharacterRenderable`
//! (`0x82871150`) builds for a NIS's characters, and the poses
//! `SceneCharacterAnim::EvalCurrAnim` publishes to them each presentation
//! frame (the render entity reads them through its presentation callbacks).
use crate::{
    attribute_runtime::AttributeRuntimeVault,
    character::{CharacterHierarchy, material_admission::CharacterMaterialAdmission, palette::CharacterPalette},
    input_cvar_runtime::InputCvarReferences,
    nis::characters::{LocalPlayerRecipe, SceneCharacter, SceneCharacterSource, ScenePose, TwistRatios, recipe_category, recipe_path},
    player::mounted_recipe::finalize::FinalizedRecipeScene,
};
use skate2_formats::{mount::MountSet, recipe::CharacterRecipe, render::AxisAlignedBox};
use std::sync::{Arc, Mutex};

/// `SceneCharacterLoader::Load` (`0x82863840`) asks `LoadSchemaFromFile` for
/// `EverythingButBlendTargetsClone` while `kNISCharacterCloneOn` (default on).
const NIS_CLONE_TYPE: u32 = 2;

/// The living world models class (`0x4e740a6670f241a7`) and the colour
/// arrays `ColorizedSkaterPresEntity` reads from it.
const MODELS_CLASS: u64 = crate::living_world_recipe_manager::LIVING_WORLD_MODELS_CLASS_ID;
const COLORIZE_RED: u64 = 0xdf76_d7d7_7385_7edb;
const COLORIZE_BLUE: u64 = 0x1202_6e2e_ed18_cc8d;

/// The scene loaders' view of the career: the mounted archives, the twist
/// ratios and the local player's recipe.
pub(crate) struct CareerSceneSource {
    mounts: Arc<MountSet>,
    ratios: TwistRatios,
    local: Mutex<Option<LocalPlayerRecipe>>,
    local_id: Mutex<Option<i32>>,
    local_index: Mutex<Option<i32>>,
    players: Mutex<std::collections::BTreeMap<i32, LocalPlayerRecipe>>,
}

impl CareerSceneSource {
    pub(crate) fn new(mounts: Arc<MountSet>, ratios: TwistRatios) -> Self {
        Self {
            mounts,
            ratios,
            local: Mutex::new(None),
            local_id: Mutex::new(None),
            local_index: Mutex::new(None),
            players: Mutex::new(Default::default()),
        }
    }

    pub(crate) fn set_local_player(&self, recipe: Option<LocalPlayerRecipe>) {
        if let Ok(mut local) = self.local.lock() {
            *local = recipe;
        }
    }

    pub(crate) fn set_players(&self, players: impl Iterator<Item = (i32, LocalPlayerRecipe)>) {
        if let Ok(mut current) = self.players.lock() { *current = players.collect(); }
    }

    /// `INetworkGameQuery::GetLocalPlayerID`: this console's player, the first player's skater.
    pub(crate) fn set_local_player_id(&self, id: i32) {
        if let Ok(mut local) = self.local_id.lock() {
            *local = Some(id);
        }
    }

    /// `SkaterManager::GetLocalIndex` as the challenge system last set it (`cMsgLocalSkaterChanged`).
    pub(crate) fn set_local_index(&self, index: i32) {
        if let Ok(mut local) = self.local_index.lock() {
            *local = Some(index);
        }
    }
}

impl SceneCharacterSource for CareerSceneSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.mounts.read_path(path).ok()
    }

    fn twist_ratios(&self) -> TwistRatios {
        self.ratios
    }

    fn local_player(&self) -> Option<LocalPlayerRecipe> {
        self.local.lock().ok().and_then(|local| local.clone())
    }

    fn local_player_id(&self) -> Option<i32> {
        self.local_id.lock().ok().and_then(|local| *local)
    }
    fn local_index(&self) -> Option<i32> {
        self.local_index.lock().ok().and_then(|local| *local)
    }
    fn player(&self, id: i32) -> Option<LocalPlayerRecipe> {
        if self.local_player_id() == Some(id) { self.local_player() }
        else { self.players.lock().ok()?.get(&id).cloned() }
    }
}

/// A scene character's renderable.
pub(crate) struct SceneCharacterModel {
    slot: usize,
    pub(crate) palette: CharacterPalette,
    /// A `SkaterPresEntity`'s skater materials.
    pub(crate) materials: Option<CharacterMaterialAdmission>,
    /// A `ColorizedSkaterPresEntity`'s `mCSColours`, its `i_colorize_red`
    /// and `i_colorize_blue`.
    pub(crate) colours: Option<[[f32; 4]; 2]>,
    /// `m_localBB`.
    local_bounds: AxisAlignedBox,
    _finalized: FinalizedRecipeScene,
    /// The pose the last presentation frame published.
    pub(crate) pose: Option<ScenePose>,
}

/// The renderables of the running scene's characters.
pub(crate) struct SceneCharacterModels {
    /// The characters (slot, schema) the renderables were built for.
    built: Vec<(usize, String)>,
    models: Vec<SceneCharacterModel>,
    /// A viewer's characters whose recipe scenes are still loading.
    pending: Vec<PendingScene>,
    /// The recipes' `tInstanceCloneType`.
    clone_type: u32,
}

/// A viewer character's recipe scene on its loader thread: the category index and the
/// finalized scene.
struct PendingScene {
    slot: usize,
    job: Option<crate::host_thread::WorkerHandle<Result<(u32, FinalizedRecipeScene), String>>>,
}

impl Default for SceneCharacterModels {
    fn default() -> Self {
        Self::with_clone_type(NIS_CLONE_TYPE)
    }
}

impl SceneCharacterModels {
    /// Renderables whose recipes load with `clone_type`, as a viewer's
    /// `MarqueeLoader` (`0x82d0b180`) gives a non-local player `CloneType_NoClone`.
    pub(crate) fn with_clone_type(clone_type: u32) -> Self {
        Self { built: Vec::new(), models: Vec::new(), pending: Vec::new(), clone_type }
    }

    /// `character`'s renderable over a recipe its caller already holds.
    pub(crate) fn with_model(character: &SceneCharacter, model: SceneCharacterModel, clone_type: u32) -> Self {
        Self { built: vec![(character.slot, character.schema.clone())], models: vec![model], pending: Vec::new(), clone_type }
    }

    /// Builds the renderables when the scene's characters change, gives each
    /// character its own, then publishes every character's pose to it.
    pub(crate) fn present(&mut self, characters: &mut [SceneCharacter], mounts: &MountSet, vault: &Arc<AttributeRuntimeVault>, cvars: &InputCvarReferences) {
        let built: Vec<_> = characters.iter().map(|c| (c.slot, c.schema.clone())).collect();
        if built != self.built {
            self.models.clear();
            for character in characters.iter().filter(|c| !c.borrowed) {
                match load_model(character, mounts, vault, cvars, self.clone_type) {
                    Ok(model) => self.models.push(model),
                    Err(error) => eprintln!("NIS: {} renderable: {error}", character.schema),
                }
            }
            self.built = built;
        }
        self.publish(characters);
    }

    /// [`Self::present`] for a model viewer, whose `MarqueeLoader` streams the character's
    /// recipe: a character shows once its scene has loaded on a loader thread.
    pub(crate) fn present_streamed(
        &mut self,
        characters: &mut [SceneCharacter],
        mounts: &Arc<MountSet>,
        vault: &Arc<AttributeRuntimeVault>,
        cvars: &InputCvarReferences,
    ) {
        let built: Vec<_> = characters.iter().map(|c| (c.slot, c.schema.clone())).collect();
        if built != self.built {
            self.models.clear();
            self.pending.clear();
            for character in characters.iter().filter(|c| !c.borrowed) {
                let (mounts, category, schema, clone_type) =
                    (Arc::clone(mounts), character.category.clone(), character.schema.clone(), self.clone_type);
                let job = crate::host_thread::spawn("sk8 viewer load", move || {
                    let path = recipe_path(&category, &schema);
                    let recipe = CharacterRecipe::parse(
                        &mounts
                            .read_path(&path)
                            .map_err(|e| format!("{path}: {e}"))?,
                    )
                    .map_err(|e| format!("{path}: {e}"))?;
                    prepare_scene(&mounts, recipe, &category, clone_type)
                })
                .ok();
                self.pending.push(PendingScene { slot: character.slot, job });
            }
            self.built = built;
        }
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].job.as_ref().is_some_and(|job| !job.is_finished()) {
                index += 1;
                continue;
            }
            let pending = self.pending.remove(index);
            let Some(character) = characters.iter().find(|c| c.slot == pending.slot) else {
                continue;
            };
            let model = match pending.job {
                Some(job) => job
                    .join()
                    .unwrap_or_else(|_| Err("scene loader panicked".to_owned()))
                    .and_then(|(category, finalized)| scene_model(character, category, finalized, vault, cvars)),
                None => load_model(character, mounts, vault, cvars, self.clone_type),
            };
            match model {
                Ok(model) => self.models.push(model),
                Err(error) => eprintln!("NIS: {} renderable: {error}", character.schema),
            }
        }
        self.publish(characters);
    }

    /// Gives each character its render entity, then publishes its pose to its renderable.
    fn publish(&mut self, characters: &mut [SceneCharacter]) {
        for model in &mut self.models {
            let Some(character) = characters.iter_mut().find(|c| c.slot == model.slot) else {
                model.pose = None;
                continue;
            };
            if !character.has_render_entity()
                && let Some(&inverse_bind) = model.palette.skinning().hierarchy_inverse_bind_pose.get(character.anim.trajectory_bone())
                && let Err(error) = character.attach_render_entity(inverse_bind)
            {
                eprintln!("NIS: {} render entity: {error}", character.schema);
            }
            model.pose = character.publish(&mut model.palette).map_err(|e| eprintln!("NIS: {} pose: {e}", character.schema)).ok();
        }
    }

    /// The renderables with a published pose, for the renderer.
    pub(crate) fn visuals(&self) -> Vec<super::SelectedSceneCharacterVisual<'_>> {
        self.models
            .iter()
            .filter_map(|m| {
                m.pose.map(|pose| {
                    // SceneRenderView::Cull 0x828501c0..e0: m_BB is m_localBB
                    // plus the m_MatLtoW translation, lane by lane.
                    let at = |c: [f32; 4]| -> [f32; 3] { std::array::from_fn(|lane| c[lane] + pose.root[12 + lane]) };
                    let local = |c: [f32; 4]| -> [f32; 3] { [c[0], c[1], c[2]] };
                    super::SelectedSceneCharacterVisual {
                        palette: &m.palette,
                        materials: m.materials.as_ref(),
                        colours: m.colours,
                        hips_position: pose.hips,
                        world_bounds: [at(m.local_bounds.minimum), at(m.local_bounds.maximum)],
                        root: pose.root,
                        local_bounds: [local(m.local_bounds.minimum), local(m.local_bounds.maximum)],
                    }
                })
            })
            .collect()
    }
}

/// `CreateCharacterRenderable` for the recipe the scene's loader read, at
/// LOD 0 (`UpdateLODs` `0x82829828` holds a NIS character there), over the
/// scene clip's hierarchy. A LivingWorld or Vehicle recipe takes a
/// `ColorizedSkaterPresEntity`; any other a `SkaterPresEntity`.
fn load_model(
    character: &SceneCharacter,
    mounts: &MountSet,
    vault: &Arc<AttributeRuntimeVault>,
    cvars: &InputCvarReferences,
    clone_type: u32,
) -> Result<SceneCharacterModel, String> {
    let path = recipe_path(&character.category, &character.schema);
    let recipe = CharacterRecipe::parse(&mounts.read_path(&path).map_err(|e| format!("{path}: {e}"))?).map_err(|e| format!("{path}: {e}"))?;
    recipe_model(character, recipe, mounts, vault, cvars, clone_type)
}

/// The renderable `load_model` builds once it has the recipe.
pub(crate) fn recipe_model(
    character: &SceneCharacter,
    recipe: CharacterRecipe,
    mounts: &MountSet,
    vault: &Arc<AttributeRuntimeVault>,
    cvars: &InputCvarReferences,
    clone_type: u32,
) -> Result<SceneCharacterModel, String> {
    let (index, finalized) = prepare_scene(mounts, recipe, &character.category, clone_type)?;
    scene_model(character, index, finalized, vault, cvars)
}

/// The recipe's scene in its category, finalized for `clone_type`, and the category index:
/// the part of `load_model` that reads only the recipe and the archives.
fn prepare_scene(
    mounts: &MountSet,
    mut recipe: CharacterRecipe,
    category: &str,
    clone_type: u32,
) -> Result<(u32, FinalizedRecipeScene), String> {
    let (index, name) = recipe_category(category);
    recipe.category = index;
    recipe.clone_type = clone_type;
    let mut scene = crate::mounted_character_scene::own_character_scene_category(mounts, &recipe, name).map_err(|e| e.to_string())?;
    scene.category = recipe.category;
    let mut finalized = FinalizedRecipeScene::new(Arc::new(scene));
    finalized.try_finalize(recipe.clone_type).map_err(|e| e.to_string())?;
    Ok((index, finalized))
}

/// The renderable over a prepared scene of the category `index`.
fn scene_model(
    character: &SceneCharacter,
    index: u32,
    finalized: FinalizedRecipeScene,
    vault: &Arc<AttributeRuntimeVault>,
    cvars: &InputCvarReferences,
) -> Result<SceneCharacterModel, String> {
    let colorized = index == 1 || index == 2;
    let hierarchy = CharacterHierarchy { bone_names: character.anim.data().database.hierarchy.bone_names.clone() };
    let scene = Arc::clone(finalized.scene().map_err(|e| e.to_string())?);
    // SkaterPresEntity::SkaterPresEntity 0x827e5740: CalculateBoundsInfo, then
    // 0x827e5a30..5b2c lowers both y lanes by 1.0 into m_localBB.
    let mut local_bounds = scene.level_bounds(0).map_err(|e| e.to_string())?.ok_or("LOD 0 model bounds")?;
    local_bounds.minimum[1] -= 1.0;
    local_bounds.maximum[1] -= 1.0;
    let palette = CharacterPalette::new(scene, &hierarchy, 0).map_err(|e| e.to_string())?;
    let (materials, colours) = if colorized {
        (None, Some(colorized_colours(vault, &character.schema)?))
    } else {
        let mut materials = CharacterMaterialAdmission::try_new(&finalized, &palette, Arc::clone(vault), cvars.clone()).map_err(|e| e.to_string())?;
        materials.try_construct_scene_character().map_err(|e| e.to_string())?;
        (Some(materials), None)
    };
    Ok(SceneCharacterModel { slot: character.slot, palette, materials, colours, local_bounds, _finalized: finalized, pose: None })
}

/// `ColorizedSkaterPresEntity::ColorizedSkaterPresEntity` (`0x827e5e20`):
/// `mCSColours` start at (0.5, 0.5, 0.5, 1); each leaf collection of the
/// living world models class named (layout offset 0) as the recipe's schema
/// sets them to element 0 of its red and blue arrays, a missing one to zero
/// (`DefaultDataArea`), the last such collection last (`0x827e5e9c..0x827e6170`).
fn colorized_colours(vault: &Arc<AttributeRuntimeVault>, schema: &str) -> Result<[[f32; 4]; 2], String> {
    let mut colours = [[0.5, 0.5, 0.5, 1.0]; 2];
    for collection in vault.leaf_collections_sorted(MODELS_CLASS).map_err(|e| e.to_string())? {
        let instance = vault.instance(MODELS_CLASS, collection);
        let name = instance.try_cached_layout_text(0).map_err(|e| e.to_string())?;
        if name.as_ref().is_none_or(|name| name.bytes() != schema.as_bytes()) {
            continue;
        }
        for (colour, key) in colours.iter_mut().zip([COLORIZE_RED, COLORIZE_BLUE]) {
            *colour = [0.0; 4];
            if let Some(data) = instance.try_vector_array_data(key, 0, 4).map_err(|e| e.to_string())? {
                for (lane, word) in colour.iter_mut().zip(data.bytes().chunks_exact(4)) {
                    *lane = f32::from_be_bytes(word.try_into().expect("four-byte lane"));
                }
            }
        }
    }
    Ok(colours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nis::{
        characters::{instance_characters, twist_ratios},
        directory::{PATH, SceneDirectory, scene_hash},
    };
    use skate2_formats::mount::{MountedBig, SKATE2_ARCHIVES};

    fn mounted() -> Arc<MountSet> {
        let root = std::path::PathBuf::from(std::env::var_os("SKATE2_GAME_ROOT").expect("game root"));
        let mut mounts = MountSet::new();
        for spec in SKATE2_ARCHIVES {
            let path = spec.path_under(&root);
            if path.is_file() {
                mounts.push(MountedBig::open(path, spec.use_external_header).unwrap());
            }
        }
        Arc::new(mounts)
    }

    const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

    #[test]
    #[ignore = "requires original SKATE2_GAME_ROOT assets"]
    fn a_current_player_scene_borrows_the_local_skater_and_poses_its_rig() {
        let mounts = mounted();
        let vault = AttributeRuntimeVault::try_load_mounted(&mounts, "data/db/skater.vlt", &crate::selected_original_asset_classes::selected_classes()).unwrap();
        let cvars = InputCvarReferences::new_static_initialized();
        let source = CareerSceneSource::new(Arc::clone(&mounts), twist_ratios(&mounts).unwrap());
        let directory = SceneDirectory::parse(&mounts.read_path(PATH).unwrap()).unwrap();
        let scene = directory.properties(scene_hash("01_Welcome_Back")).unwrap().clone();
        // Without a local player the record is left out.
        assert!(instance_characters(directory.characters(&scene), &[], &[], &source).iter().all(|c| !c.borrowed));
        // The default career skater: the marquee tutorial_male.
        source.set_local_player(Some(LocalPlayerRecipe { schema: "tutorial_male".into(), create_a_character: None }));
        let mut characters = instance_characters(directory.characters(&scene), &[], &[], &source);
        for character in &mut characters {
            character.set_scene_transform(IDENTITY);
            character.set_time(1.0).unwrap();
        }
        // The scene builds no renderable for it.
        let mut models = SceneCharacterModels::default();
        models.present(&mut characters, &mounts, &vault, &cvars);
        assert_eq!(models.visuals().len(), characters.len() - 1);
        let borrowed = characters.iter_mut().find(|c| c.borrowed).expect("current_player");
        assert_eq!((borrowed.schema.as_str(), borrowed.category.as_str()), ("tutorial_male", "marquee"));
        assert!(!borrowed.has_render_entity());
        // The local skater's own renderable takes the clip's pose, and its
        // local-to-world matrix is the object matrix the camera reads.
        let mut skater = load_model(borrowed, &mounts, &vault, &cvars, NIS_CLONE_TYPE).unwrap();
        borrowed.attach_render_entity(skater.palette.skinning().hierarchy_inverse_bind_pose[borrowed.anim.trajectory_bone()]).unwrap();
        let pose = borrowed.publish(&mut skater.palette).unwrap();
        eprintln!("current_player hips {:?}", pose.hips);
        assert!(pose.hips.iter().zip(&pose.root[12..15]).all(|(h, r)| h.is_finite() && (h - r).abs() < 1e-3), "{pose:?}");
        assert_eq!(borrowed.object_transform(), Some(pose.root));
        borrowed.add_time(0.5).unwrap();
        assert_eq!(borrowed.object_transform(), Some(borrowed.publish(&mut skater.palette).unwrap().root));
    }

    #[test]
    #[ignore = "requires original SKATE2_GAME_ROOT assets"]
    fn a_story_camera_anchored_on_a_scene_character_resolves_through_its_render_entity() {
        use crate::nis::{
            camera::{self, IceCameras},
            manager::{CameraAnchors, CameraTracks, SceneFrame},
        };
        let mounts = mounted();
        let vault = AttributeRuntimeVault::try_load_mounted(&mounts, "data/db/skater.vlt", &crate::selected_original_asset_classes::selected_classes()).unwrap();
        let cvars = InputCvarReferences::new_static_initialized();
        let source = CareerSceneSource::new(Arc::clone(&mounts), twist_ratios(&mounts).unwrap());
        let directory = SceneDirectory::parse(&mounts.read_path(PATH).unwrap()).unwrap();
        // RedaCarRide_07's take anchors its camera on avatar 7: entity 1,
        // Haslam (ai_skater_01, marquee). Entity 0 is Reda's car, a Vehicle
        // recipe's ColorizedSkaterPresEntity.
        let props = directory.properties(scene_hash("RedaCarRide_07")).unwrap().clone();
        let frame = SceneFrame { location: props.location, orientation: props.orientation, normal: [0.0, 1.0, 0.0] };
        let m = skate2_formats::ice_camera::compute_transform(frame.location, frame.orientation, frame.normal);
        let mut characters = instance_characters(directory.characters(&props), &[], &[], &source);
        for character in &mut characters {
            character.set_scene_transform(std::array::from_fn(|i| m[i / 4][i % 4]));
            character.set_time(0.0).unwrap();
        }
        let mut models = SceneCharacterModels::default();
        models.present(&mut characters, &mounts, &vault, &cvars);
        assert_eq!((characters[0].schema.as_str(), characters[1].schema.as_str()), ("reda_car", "ai_skater_01"));
        assert!(characters[0].has_render_entity() && characters[1].has_render_entity());
        // The two pedestrians are ColorizedSkaterPresEntity renderables; the
        // minivan (filed as livingworld, its recipe is not there) is no entity.
        let schemas: Vec<_> = characters.iter().map(|c| c.schema.as_str()).collect();
        assert_eq!(schemas, ["reda_car", "ai_skater_01", "female_business_1", "male_adult_2"]);
        let visuals = models.visuals();
        assert_eq!(visuals.len(), 4);
        // No living world models collection is named reda_car, so the car
        // keeps the constructor's colours; its hips are the Chassis bone.
        let car = &visuals[0];
        assert!(car.materials.is_none() && car.colours == Some([[0.5, 0.5, 0.5, 1.0]; 2]));
        assert!([0, 2].into_iter().all(|lane| (car.world_bounds[0][lane]..=car.world_bounds[1][lane]).contains(&car.hips_position[lane])));
        assert_eq!(car.world_bounds[0], std::array::from_fn(|lane| car.local_bounds[0][lane] + car.root[12 + lane]));
        assert!(visuals[1].materials.is_some() && visuals[1].colours.is_none());
        for visual in &visuals[2..] {
            let colours = visual.colours.expect("colorized");
            eprintln!("colorized colours {colours:?}, bounds {:?}", visual.world_bounds);
            assert!(visual.materials.is_none() && colours != [[0.5, 0.5, 0.5, 1.0]; 2]);
            assert!((0..3).all(|lane| visual.world_bounds[0][lane] <= visual.hips_position[lane] + 1.0));
        }
        let cameras = IceCameras::new(Arc::new(skate2_formats::ice_camera::CameraSet::parse(&mounts.read_path(camera::PATH).unwrap()).unwrap()));
        let (mut track, _) = cameras.connect(&props, 0, None).unwrap();
        let local = Some([0.0; 3]);
        let mut mover = skate2_formats::ice_camera::Mover::default();
        let (mut time, mut frames, mut needs_entity) = (0.0f32, 0, 0);
        loop {
            let update = track.update(time);
            if let Some(set) = update.set_time {
                time = set;
                characters.iter_mut().for_each(|c| c.set_time(time).unwrap());
            }
            if update.finished {
                break;
            }
            let entities = characters.iter().map(|c| c.object_transform().map(|m| [m[12], m[13], m[14]])).collect();
            let anchors = CameraAnchors { anchor: local, axes: None, hips: local, entities, ..CameraAnchors::default() };
            assert!(track.camera(&frame, &anchors, &mut mover, 1.0 / 60.0).is_some(), "{time}: {:?}", track.camera_error());
            let bare = CameraAnchors { anchor: local, axes: None, hips: local, entities: Vec::new(), ..CameraAnchors::default() };
            let haslam = anchors.entities[1].unwrap();
            let without = track.camera(&frame, &CameraAnchors { entities: vec![None; 2], ..bare }, &mut mover, 1.0 / 60.0);
            if without.is_none() {
                needs_entity += 1;
                // The eye sits off Haslam's object matrix, within a few metres.
                let eye = track.camera(&frame, &anchors, &mut mover, 1.0 / 60.0).unwrap().matrix;
                let d = (0..3).map(|i| (eye[12 + i] - haslam[i]).powi(2)).sum::<f32>().sqrt();
                assert!(d < 20.0, "{time}: eye {:?} haslam {haslam:?}", &eye[12..15]);
            }
            frames += 1;
            time += 1.0 / 60.0;
            characters.iter_mut().for_each(|c| c.add_time(1.0 / 60.0).unwrap());
        }
        // The whole take is anchored on him.
        assert!(frames > 400 && needs_entity == frames, "{needs_entity} of {frames}");
    }

    #[test]
    #[ignore = "requires original SKATE2_GAME_ROOT assets"]
    fn reda_car_ride_cameras_anchor_on_the_car() {
        use crate::nis::{
            camera::{self, IceCameras},
            manager::{CameraAnchors, CameraTracks, SceneFrame},
        };
        let mounts = mounted();
        let vault = AttributeRuntimeVault::try_load_mounted(&mounts, "data/db/skater.vlt", &crate::selected_original_asset_classes::selected_classes()).unwrap();
        let cvars = InputCvarReferences::new_static_initialized();
        let source = CareerSceneSource::new(Arc::clone(&mounts), twist_ratios(&mounts).unwrap());
        let directory = SceneDirectory::parse(&mounts.read_path(PATH).unwrap()).unwrap();
        let cameras = IceCameras::new(Arc::new(skate2_formats::ice_camera::CameraSet::parse(&mounts.read_path(camera::PATH).unwrap()).unwrap()));
        for name in ["RedaCarRide_01", "RedaCarRide_05"] {
            let props = directory.properties(scene_hash(name)).unwrap().clone();
            let frame = SceneFrame { location: props.location, orientation: props.orientation, normal: [0.0, 1.0, 0.0] };
            let m = skate2_formats::ice_camera::compute_transform(frame.location, frame.orientation, frame.normal);
            let mut characters = instance_characters(directory.characters(&props), &[], &[], &source);
            for character in &mut characters {
                character.set_scene_transform(std::array::from_fn(|i| m[i / 4][i % 4]));
                character.set_time(0.0).unwrap();
            }
            let mut models = SceneCharacterModels::default();
            models.present(&mut characters, &mounts, &vault, &cvars);
            let car = characters.iter().position(|c| c.schema == "reda_car").expect("reda_car");
            assert!(characters[car].has_render_entity());
            let (mut track, _) = cameras.connect(&props, 0, None).unwrap();
            let local = Some([0.0; 3]);
            let mut mover = skate2_formats::ice_camera::Mover::default();
            let (mut time, mut frames, mut needs_car, mut unresolved) = (0.0f32, 0, 0, 0);
            loop {
                let update = track.update(time);
                if let Some(set) = update.set_time {
                    time = set;
                    characters.iter_mut().for_each(|c| c.set_time(time).unwrap());
                }
                if update.finished {
                    break;
                }
                let entities: Vec<_> = characters.iter().map(|c| c.object_transform().map(|m| [m[12], m[13], m[14]])).collect();
                let mut without = entities.clone();
                without[car] = None;
                let anchors = CameraAnchors { anchor: local, axes: None, hips: local, entities, ..CameraAnchors::default() };
                if track.camera(&frame, &anchors, &mut mover, 1.0 / 60.0).is_none() {
                    unresolved += 1;
                } else if track.camera(&frame, &CameraAnchors { entities: without, ..anchors }, &mut mover, 1.0 / 60.0).is_none() {
                    needs_car += 1;
                }
                frames += 1;
                time += 1.0 / 60.0;
                characters.iter_mut().for_each(|c| c.add_time(1.0 / 60.0).unwrap());
            }
            eprintln!("{name}: {frames} frames, {needs_car} on the car, {unresolved} unresolved");
            assert!(unresolved == 0 && needs_car > 0, "{name}: {unresolved} unresolved, {needs_car} on the car");
        }
    }

    #[test]
    #[ignore = "requires original SKATE2_GAME_ROOT assets"]
    fn the_first_career_scene_builds_and_poses_its_renderable() {
        let mounts = mounted();
        let vault = AttributeRuntimeVault::try_load_mounted(&mounts, "data/db/skater.vlt", &crate::selected_original_asset_classes::selected_classes()).unwrap();
        let cvars = InputCvarReferences::new_static_initialized();
        let source = CareerSceneSource::new(Arc::clone(&mounts), twist_ratios(&mounts).unwrap());
        let directory = SceneDirectory::parse(&mounts.read_path(PATH).unwrap()).unwrap();
        let scene = directory.properties(scene_hash("tutorial_m_1a_goto_slappys")).unwrap().clone();
        let mut characters = instance_characters(directory.characters(&scene), &[], &[], &source);
        for character in &mut characters {
            character.set_scene_transform(IDENTITY);
            character.set_time(1.0).unwrap();
        }
        let mut models = SceneCharacterModels::default();
        models.present(&mut characters, &mounts, &vault, &cvars);
        let visuals = models.visuals();
        assert_eq!(visuals.len(), 1);
        let hips = visuals[0].hips_position;
        eprintln!("rob_dyrdek hips {hips:?}");
        // The scene sits at the origin, so the hips follow the clip's root
        // near (1707, 110, -578).
        assert!((hips[0] - 1708.0).abs() < 25.0 && (hips[2] + 578.0).abs() < 25.0, "{hips:?}");
        // A new presentation frame keeps the renderable and poses it again;
        // the character's object matrix moves with it.
        let object = characters[0].object_transform().expect("render entity");
        characters[0].add_time(0.5).unwrap();
        assert_ne!(characters[0].object_transform(), Some(object));
        models.present(&mut characters, &mounts, &vault, &cvars);
        assert_ne!(models.visuals()[0].hips_position, hips);
    }
}
