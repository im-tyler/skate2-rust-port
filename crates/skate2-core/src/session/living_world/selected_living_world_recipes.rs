//! RecipeMan -> existing SimCharacterManager requests -> SAME mounted registry.
//! A missing recipe decodes on a host worker; its completion is delivered a
//! fixed number of RecipeMan updates later, only after Load has stored its
//! request ID, with the request slot still occupied during callback.
//! Native worker/queue timing is not claimed by this selected schedule.
use crate::{
    attribute_runtime::AttributeRuntimeVault,
    input_cvar_runtime::InputCvarReferences,
    living_world_grind_services::{
        LivingWorldGrindServices, character_models::CharacterModelError,
    },
    living_world_recipe_manager::{
        attributes::{RecipeAttributeError, RecipeAttributes},
        runtime::{RecipeInitError, RecipeMan, RecipeModel, RecipeRuntimeError, RecipeServices},
    },
    physical_player_assembly::PhysicalAssemblyWorld,
    player::{
        mounted_recipe::{
            MountedRecipeError, MountedRecipeRegistry,
            ambient::{AmbientBegin, AmbientRecipeAssets, PreparedAmbientRecipe},
        },
        requests::{LoadTicket, RequestError, SimCharacterRequests, UnloadTicket},
    },
};
use skate2_formats::mount::MountSet;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
#[path = "selected_living_world_pedestrian_animation.rs"]
pub(crate) mod pedestrian_animation;
#[path = "selected_living_world_props.rs"]
pub(crate) mod props;
#[path = "selected_living_world_dmo_props.rs"]
mod dmo_props;
#[path = "selected_living_world_dmo_prop_activation.rs"]
mod dmo_prop_activation;
#[path = "selected_living_world_prop_census.rs"]
mod prop_census;
#[path = "selected_living_world_dmo_prestate.rs"]
mod dmo_prestate;
#[path = "selected_living_world_dmo_positional.rs"]
mod dmo_positional;
#[path = "selected_living_world_dmo_reset.rs"]
pub(crate) mod dmo_reset;
#[path = "selected_living_world_challenge_dmos.rs"]
pub(crate) mod challenge_dmos;
#[path = "selected_living_world_pedestrian_spawn.rs"]
mod pedestrian_spawn;
#[path = "selected_living_world_pedestrian_census.rs"]
mod pedestrian_census;
#[path = "selected_living_world_pedestrian_cull.rs"]
mod pedestrian_cull;
#[path = "selected_living_world_pedestrian_execution.rs"]
mod pedestrian_execution;
#[path = "selected_living_world_pedestrian_moodables.rs"]
mod pedestrian_moodables;
#[path = "selected_living_world_transfer.rs"]
mod transfer;
#[path = "selected_living_world_mood.rs"]
pub(crate) mod mood;

#[derive(Debug)]
pub(crate) enum RecipeBridgeError {
    Attribute(RecipeAttributeError),
    Request(RequestError),
    Mounted(MountedRecipeError),
    Model(CharacterModelError),
    Missing(&'static str),
    PresentationCapacity,
    AssetUsersPending(u64),
    Worker(String),
}
#[derive(Debug)]
pub(crate) enum SelectedLivingWorldRecipeError {
    Init(RecipeInitError),
    Runtime(RecipeRuntimeError<RecipeBridgeError>),
    Request(RequestError),
    LivingWorldLock,
}
impl std::fmt::Display for SelectedLivingWorldRecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LivingWorld recipes: {self:?}")
    }
}

/// Native presentation ModelManager's 128-entry recipe membership. It points
/// at the same character assets; it never decodes or owns a second registry.
/// AddRecipe82810748, ReceiveDRegLWSchemaMsg8280dfe0.
pub(crate) struct PresentationRecipeModels {
    entries: BTreeMap<u64, Arc<AmbientRecipeAssets>>,
}
impl PresentationRecipeModels {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
    pub(crate) fn get(&self, id: u64) -> Option<&Arc<AmbientRecipeAssets>> {
        self.entries.get(&id)
    }
    fn add(&mut self, id: u64, registry: &MountedRecipeRegistry) -> Result<(), RecipeBridgeError> {
        // GetRecipe precedes the membership test in the source.
        let assets = registry
            .ambient_assets(id)
            .map_err(RecipeBridgeError::Mounted)?;
        if self.entries.contains_key(&id) {
            return Ok(());
        }
        if self.entries.len() == 128 {
            return Err(RecipeBridgeError::PresentationCapacity);
        }
        self.entries.insert(id, Arc::clone(assets));
        Ok(())
    }
}
struct LoadedRecipe {
    ticket: LoadTicket,
    asset: u64,
}
/// RecipeMan updates between a missing recipe's request and its completion: the
/// earliest a load is delivered. A decode still running then is delivered on the
/// first update after it ends, so a slow disk delays the pedestrian, never the
/// simulation (RecipeMan's WaitingForLoad states hold it as the original's
/// streamed loads were held).
const AMBIENT_LOAD_UPDATES: u64 = 8;
struct PendingRecipe {
    ticket: LoadTicket,
    due: u64,
    job: crate::host_thread::WorkerHandle<Result<PreparedAmbientRecipe, String>>,
}
/// `SimCharacterManager::Unload` 0x82817910 of a recipe: the request completes
/// (`OnUnloaded` 0x828154a0 -> RecipeMan) on the next RecipeMan update.
struct PendingUnload {
    ticket: UnloadTicket,
    asset: u64,
    due: u64,
}
pub(crate) struct SelectedLivingWorldRecipes {
    pub(crate) vault: Option<Arc<AttributeRuntimeVault>>,
    pub(crate) challenge_avoid_volumes: crate::pedestrian_navigation::wander::ChallengeAvoidVolumes,
    pub(crate) recipes: RecipeMan,
    pub(crate) presentation: PresentationRecipeModels,
    callback: Arc<()>,
    completed: VecDeque<LoadedRecipe>,
    pending: VecDeque<PendingRecipe>,
    unloads: VecDeque<PendingUnload>,
    /// `cMsgAssetSimDRegLWSchema` recipes `RecipeMan::UnloadLivingWorldModel` 0x82712918
    /// broadcast, not yet taken by the challenge system's character manager.
    pub(crate) deregistered: Vec<u64>,
    updates: u64,
    /// Loads are delivered at their due update, waiting for decodes still running, in the
    /// loading screen's loop (and throughout a traced run): a game starts the same on any disk.
    exact_delivery: bool,
}
impl SelectedLivingWorldRecipes {
    pub(crate) fn new() -> Self {
        Self {
            vault: None,
            challenge_avoid_volumes: crate::pedestrian_navigation::wander::ChallengeAvoidVolumes::new(),
            recipes: RecipeMan::new(),
            presentation: PresentationRecipeModels::new(),
            callback: Arc::new(()),
            completed: VecDeque::new(),
            pending: VecDeque::new(),
            unloads: VecDeque::new(),
            deregistered: Vec::new(),
            updates: 0,
            exact_delivery: false,
        }
    }
    /// GameModule.SecondaryLoad03 calls Init827289f8: mappings and queues only.
    pub(crate) fn initialize(
        &mut self,
        vault: &Arc<AttributeRuntimeVault>,
        cvars: &InputCvarReferences,
    ) -> Result<(), SelectedLivingWorldRecipeError> {
        self.recipes
            .initialize(&mut RecipeAttributes::new(vault, cvars))
            .map_err(SelectedLivingWorldRecipeError::Init)?;
        self.vault = Some(Arc::clone(vault));
        Ok(())
    }
}

impl SelectedLivingWorldRecipes {
    /// Advance the original RecipeMan and its real asset completions without
    /// requiring an Actor/animation frame. GameModule drives this during load
    /// as well as Simulation::SimUpdate1 during gameplay.
    fn update_models(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        vault: &Arc<AttributeRuntimeVault>,
        registry: &mut MountedRecipeRegistry,
        requests: &mut SimCharacterRequests,
        mounts: &Arc<MountSet>,
    ) -> Result<(), SelectedLivingWorldRecipeError> {
        use SelectedLivingWorldRecipeError as E;
        let mut living = world
            .grind_services
            .living_world()
            .lock()
            .map_err(|_| E::LivingWorldLock)?;
        let runtime = &mut *self;
        runtime.updates += 1;
        let exact = runtime.exact_delivery || crate::diagnostics::traced_run();
        let mut bridge = Bridge {
            attributes: RecipeAttributes::new(vault, cvars),
            registry,
            requests,
            mounts,
            living: &mut living,
            presentation: &mut runtime.presentation,
            callback: &runtime.callback,
            completed: &mut runtime.completed,
            pending: &mut runtime.pending,
            unloads: &mut runtime.unloads,
            deregistered: &mut runtime.deregistered,
            updates: runtime.updates,
        };
        runtime.recipes.update(&mut bridge).map_err(E::Runtime)?;
        let failed = |error| E::Runtime(RecipeRuntimeError::Service(error));
        while bridge.pending.front().is_some_and(|load| load.due <= bridge.updates && (exact || load.job.is_finished())) {
            let load = bridge.pending.pop_front().expect("due recipe load");
            if load.due < bridge.updates && trace_pedestrians() {
                eprintln!("pedestrian recipe delivered {} updates after it was due", bridge.updates - load.due);
            }
            let prepared = load
                .job
                .join()
                .map_err(|_| failed(RecipeBridgeError::Worker("recipe decode worker panicked".to_owned())))?
                .map_err(|error| failed(RecipeBridgeError::Worker(error)))?;
            let asset = bridge
                .registry
                .finish_ambient(prepared)
                .map_err(|error| failed(RecipeBridgeError::Mounted(error)))?;
            bridge.completed.push_back(LoadedRecipe { ticket: load.ticket, asset });
        }
        // The source callback enters after the request ID store. Do not call
        // loaded() recursively from RecipeServices.load(), or free its slot early.
        while let Some(done) = bridge.completed.pop_front() {
            runtime
                .recipes
                .loaded(done.ticket.id(), done.asset, &mut bridge)
                .map_err(E::Runtime)?;
            bridge
                .requests
                .finish_loaded_callback(&done.ticket)
                .map_err(E::Request)?;
        }
        while bridge.unloads.front().is_some_and(|unload| unload.due <= bridge.updates) {
            let unload = bridge.unloads.pop_front().expect("due recipe unload");
            // CharacterAssetManager drops the schema's reference, and its registry
            // membership with the last one, before the request's OnUnloaded.
            if bridge
                .registry
                .begin_ambient_release(unload.asset)
                .map_err(|error| failed(RecipeBridgeError::Mounted(error)))?
            {
                bridge
                    .registry
                    .finish_ambient_release(unload.asset)
                    .map_err(|error| failed(RecipeBridgeError::Mounted(error)))?;
            }
            runtime
                .recipes
                .unloaded::<RecipeBridgeError>(unload.ticket.id())
                .map_err(E::Runtime)?;
            bridge
                .requests
                .finish_unloaded_callback(&unload.ticket)
                .map_err(E::Request)?;
        }
        Ok(())
    }

    /// OneOffLoad8273add8 / GameLoad8273be50 pump the
    /// Required/Initial queues before admitting gameplay. Run no physics,
    /// population, RNG or challenge update here. Reuse the request IDs,
    /// callbacks and SIM/PRES model registration of the gameplay path.
    pub(crate) fn finish_initial_load(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        vault: &Arc<AttributeRuntimeVault>,
        registry: &mut MountedRecipeRegistry,
        requests: &mut SimCharacterRequests,
        mounts: &Arc<MountSet>,
    ) -> Result<(), SelectedLivingWorldRecipeError> {
        let exact = std::mem::replace(&mut self.exact_delivery, true);
        let mut result = Ok(());
        while result.is_ok() && self.recipes.initial_load_pending() {
            result = self.update_models(world, cvars, vault, registry, requests, mounts);
            // Original loading loop yields for 1ms; this is loading work,
            // not 120 simulation frames between model requests.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.exact_delivery = exact;
        result
    }
}

/// `SKATE2_TRACE_PEDESTRIANS`: RecipeMan's state, and the loads delivered after they were due.
fn trace_pedestrians() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("SKATE2_TRACE_PEDESTRIANS").is_some())
}

/// Borrow of existing session owners, carried to SimUpdate1's RecipeMan call.
pub(crate) struct SelectedRecipeFrame<'a> {
    /// Borrowed for ChaseMan.Update's synchronous career queries and mutations.
    pub(crate) challenge: Option<&'a mut crate::challenge::system::World>,
    pub(crate) triggers: &'a Arc<std::sync::Mutex<crate::trigger_camera_group::CameraTriggerGroup>>,
    pub(crate) pedestrian_animation: pedestrian_animation::PedestrianSessionAnimation<'a>,
    pub(crate) runtime: &'a mut SelectedLivingWorldRecipes,
    pub(crate) registry: &'a mut MountedRecipeRegistry,
    pub(crate) requests: &'a mut SimCharacterRequests,
    pub(crate) mounts: &'a Arc<MountSet>,
    pub(crate) vault: &'a Arc<AttributeRuntimeVault>,
    pub(crate) graphs: &'a crate::actor_construction::graph_startup::ActorGraphStartupOwners,
    pub(crate) painter: &'a Arc<std::sync::Mutex<crate::world_painter_layer::WorldPainterLayerMan>>,
}
impl SelectedRecipeFrame<'_> {
    pub(crate) fn reborrow(&mut self) -> SelectedRecipeFrame<'_> {
        SelectedRecipeFrame {
            challenge: self.challenge.as_deref_mut(),
            triggers: self.triggers,
            pedestrian_animation: self.pedestrian_animation,
            runtime: self.runtime,
            registry: self.registry,
            requests: self.requests,
            mounts: self.mounts,
            vault: self.vault,
            graphs: self.graphs,
            painter: self.painter,
        }
    }
    pub(crate) fn evaluate_vehicles(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        inputs: &crate::living_world_grind_services::dmo_objects::DmoCensusInputs,
        cull: bool,
        spawn: bool,
    ) -> Result<(), crate::living_world_grind_services::vehicle_entity::frame::VehicleFrameError>
    {
        use crate::living_world_grind_services::{
            census::vehicle::VehicleCensusFrame, vehicle_entity::frame::error,
        };
        let owner = Arc::clone(world.grind_services.living_world());
        let mut living = owner.lock().map_err(|_| error("LivingWorld lock"))?;
        let painter = self
            .painter
            .lock()
            .map_err(|_| error("WorldPainter lock"))?;
        VehicleCensusFrame {
            living: &mut living,
            recipe_man: &self.runtime.recipes,
            mounted_recipes: self.registry,
            vault: self.vault,
            cvars,
            graphs: self.graphs,
            world,
            painter: &painter,
            override_model_name: false,
        }
        .evaluate(
            inputs,
            cull,
            spawn,
            &mut crate::reckoning_numeric::HostReckoningNumeric,
        )
    }
    pub(crate) fn update(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
    ) -> Result<(), SelectedLivingWorldRecipeError> {
        self.runtime.update_models(world, cvars, self.vault, self.registry,
            self.requests, self.mounts)
    }

    /// The RecipeMan calls of a caller outside its update, on the same owners.
    fn with_recipe_man<R>(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        run: impl FnOnce(&mut RecipeMan, &mut Bridge<'_>) -> Result<R, RecipeRuntimeError<RecipeBridgeError>>,
    ) -> Result<R, SelectedLivingWorldRecipeError> {
        use SelectedLivingWorldRecipeError as E;
        let mut living = world
            .grind_services
            .living_world()
            .lock()
            .map_err(|_| E::LivingWorldLock)?;
        let runtime = &mut *self.runtime;
        let mut bridge = Bridge {
            attributes: RecipeAttributes::new(self.vault, cvars),
            registry: self.registry,
            requests: self.requests,
            mounts: self.mounts,
            living: &mut living,
            presentation: &mut runtime.presentation,
            callback: &runtime.callback,
            completed: &mut runtime.completed,
            pending: &mut runtime.pending,
            unloads: &mut runtime.unloads,
            deregistered: &mut runtime.deregistered,
            updates: runtime.updates,
        };
        run(&mut runtime.recipes, &mut bridge).map_err(E::Runtime)
    }

    /// `RecipeMan::LoadLivingWorldModel(modelKey)` 0x82726210 -> 0x827239e0.
    pub(crate) fn load_living_world_model(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        key: u64,
    ) -> Result<bool, SelectedLivingWorldRecipeError> {
        self.with_recipe_man(world, cvars, |recipes, bridge| recipes.load_key(key, bridge))
    }

    /// `RecipeMan::UnloadLivingWorldModel(modelName)` 0x82715278 of the model's filename.
    pub(crate) fn unload_living_world_model(
        &mut self,
        world: &mut PhysicalAssemblyWorld,
        cvars: &InputCvarReferences,
        key: u64,
    ) -> Result<(), SelectedLivingWorldRecipeError> {
        self.with_recipe_man(world, cvars, |recipes, bridge| {
            let model = bridge.model(key).map_err(RecipeRuntimeError::Service)?;
            recipes.release(&model.filename, bridge)
        })
    }
}
struct Bridge<'a> {
    attributes: RecipeAttributes<'a>,
    registry: &'a mut MountedRecipeRegistry,
    requests: &'a mut SimCharacterRequests,
    mounts: &'a Arc<MountSet>,
    living: &'a mut LivingWorldGrindServices,
    presentation: &'a mut PresentationRecipeModels,
    callback: &'a Arc<()>,
    completed: &'a mut VecDeque<LoadedRecipe>,
    pending: &'a mut VecDeque<PendingRecipe>,
    unloads: &'a mut VecDeque<PendingUnload>,
    deregistered: &'a mut Vec<u64>,
    updates: u64,
}
impl RecipeServices for Bridge<'_> {
    type Error = RecipeBridgeError;
    fn model(&mut self, key: u64) -> Result<RecipeModel, Self::Error> {
        self.attributes
            .model(key)
            .map_err(RecipeBridgeError::Attribute)
    }
    fn load(&mut self, category: u32, filename: &str) -> Result<u32, Self::Error> {
        let ticket = self
            .requests
            .begin_ambient_load(Arc::clone(self.callback))
            .map_err(RecipeBridgeError::Request)?;
        let id = ticket.id();
        let marquee;
        let filename = if category == 0 {
            marquee = marquee_schema_name(self.attributes.vault, filename)?;
            marquee.as_str()
        } else {
            filename
        };
        match self
            .registry
            .begin_ambient(category, filename, self.attributes.cvars)
            .map_err(RecipeBridgeError::Mounted)?
        {
            AmbientBegin::Cached(asset) => self.completed.push_back(LoadedRecipe { ticket, asset }),
            AmbientBegin::Load(request) => {
                let mounts = Arc::clone(self.mounts);
                let job = crate::host_thread::spawn("sk8 recipe decode", move || {
                    request.prepare(&mounts).map_err(|error| error.to_string())
                })
                .map_err(|error| RecipeBridgeError::Worker(error.to_string()))?;
                self.pending.push_back(PendingRecipe {
                    ticket,
                    due: self.updates + AMBIENT_LOAD_UPDATES,
                    job,
                });
            }
        }
        Ok(id)
    }
    fn register_simulation(&mut self, asset: u64, key: u64) -> Result<(), Self::Error> {
        let assets = self
            .registry
            .ambient_assets(asset)
            .map_err(RecipeBridgeError::Mounted)?;
        self.living
            .dmo_models
            .add_recipe(
                self.attributes.vault,
                asset,
                key,
                &assets.finalized,
                &assets.collision,
            )
            .map_err(RecipeBridgeError::Model)
    }
    fn register_presentation(&mut self, asset: u64, _key: u64) -> Result<(), Self::Error> {
        self.presentation.add(asset, self.registry)
    }
    fn deregister_simulation(&mut self, asset: u64) -> Result<(), Self::Error> {
        // RemoveRecipe82ce8cd0 notifies the entity managers before the model
        // leaves (Manager.NotifyOfAssetRemoval826f5508). Pedestrian, Vehicle
        // and Prop ignore it (LWBasicEntity slot 0x6c is the empty 825187c0);
        // DynamicObject.NotifyOfAssetRemoval82d2a400 releases the model of an
        // object built from the asset, which is not ported.
        let objects = self.living.dmo_objects.as_deref();
        self.living.dmo_models.remove_recipe(asset, &mut |asset| {
            let used = objects.is_some_and(|objects| {
                objects.ids().into_iter().any(|id| {
                    objects.object(id).is_some_and(|object| object.model.asset_id == asset)
                })
            });
            if used {
                Err(RecipeBridgeError::AssetUsersPending(asset))
            } else {
                Ok(())
            }
        })?;
        // The same cMsgAssetSimDRegLWSchema reaches
        // cChallengeCharacterManager.ReceiveDRegLWSchemaMsg825a8470.
        self.deregistered.push(asset);
        Ok(())
    }
    fn deregister_presentation(&mut self, asset: u64) -> Result<(), Self::Error> {
        self.presentation.entries.remove(&asset);
        // ReceiveDRegLWSchemaMsg8280dfe0 then destroys the presentation
        // entities built from the recipe (NotifyOfAssetRemoval8280cbe8). A
        // live pedestrian still built from it would need that destruction,
        // which is not ported; a vehicle without its recipe is not presented.
        let peds = self
            .living
            .pedestrian_entities
            .as_ref()
            .ok_or(RecipeBridgeError::Missing("pedestrian entities"))?;
        let used = peds.ids().into_iter().any(|id| {
            peds.get(id)
                .and_then(|ped| ped.core.as_ref())
                .is_some_and(|core| core.recipe_id == asset)
        });
        if used {
            return Err(RecipeBridgeError::AssetUsersPending(asset));
        }
        Ok(())
    }
    fn unload(&mut self, asset: u64) -> Result<u32, Self::Error> {
        // SimCharacterManager.Unload82817910 takes a request in the same slots
        // as Load; the unload and its OnUnloaded follow on the next update.
        let ticket = self
            .requests
            .begin_unload(Arc::clone(self.callback))
            .map_err(RecipeBridgeError::Request)?;
        let id = ticket.id();
        self.unloads.push_back(PendingUnload {
            ticket,
            asset,
            due: self.updates + 1,
        });
        Ok(id)
    }
}

/// The schema a Marquee request loads: ReceiveSimCharacterLoadRequest8281b6f0
/// passes `MarqueeDefs::GetIDForName`82cfe828 of the model name (the first of
/// `mCharacterMarqueeList`, the class leaf collections, whose layout name
/// matches; -1 without one) to CreateMarqueeLoader82814998, which takes
/// `GetDefaultMarquee` unless that `characters_marquee` layout byte 8 is set;
/// MarqueeLoader82d0b110 loads `GetSkaterName`82cfe920 of it.
pub(crate) fn marquee_schema_name(
    vault: &Arc<AttributeRuntimeVault>,
    name: &str,
) -> Result<String, RecipeBridgeError> {
    use crate::ai::skater_manager::CHARACTERS_MARQUEE;
    let attribute = |error: crate::attribute_runtime::AttributeRuntimeError| {
        RecipeBridgeError::Attribute(error.into())
    };
    let text = |id: u64| -> Result<Option<String>, RecipeBridgeError> {
        let instance = vault.instance(CHARACTERS_MARQUEE, id);
        if instance.is_null() {
            return Ok(None);
        }
        Ok(instance
            .try_cached_layout_text(0)
            .map_err(attribute)?
            .map(|text| String::from_utf8_lossy(text.bytes()).into_owned()))
    };
    let mut id = u64::MAX;
    for candidate in vault.leaf_collections_sorted(CHARACTERS_MARQUEE).map_err(attribute)? {
        if text(candidate)?.as_deref() == Some(name) {
            id = candidate;
            break;
        }
    }
    let usable = vault
        .instance(CHARACTERS_MARQUEE, id)
        .try_cached_layout_field(8, 1)
        .map_err(attribute)?
        .is_some_and(|byte| byte.bytes()[0] != 0);
    if !usable {
        id = crate::player::DEFAULT_MARQUEE_ID;
    }
    Ok(text(id)?.unwrap_or_else(|| "Invalid".to_owned()))
}
