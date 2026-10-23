//! Selected session asset-service ownership over the existing resident registry.
//! This does not construct a second ID map or certify the whole asset manager.
use crate::{
    collection_resident_streamer::CollectionId,
    simulation_inner_registry_asset::{InnerAssetError, SimulationInnerRegistryAsset},
    simulation_normal_arena_plan::{SimulationNormalArenaPlan, SimulationNormalOperation},
    simulation_resident_registry::{
        SimulationInnerRegistryError, SimulationResidentRegistry, SimulationResidentRegistryError,
    },
};
use std::sync::{Arc, Mutex};
#[path = "selected_simulation_registry_bridge.rs"]
pub(crate) mod bridge;
#[path = "selected_presentation_navigation.rs"]
mod presentation_navigation;
use bridge::{RegistryBinding, SelectedAssetPublicationSink, StandaloneSink};

type SessionRegistry = SimulationResidentRegistry<
    SimulationNormalArenaPlan,
    fn(SimulationResidentRegistryError) -> SimulationResidentRegistryError,
    SimulationResidentRegistryError,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedAssetError {
    Lock,
    Allocation,
    PendingPreviousFailure,
    RequiresResidentPublication,
    DetachedResidentArena,
    Source(InnerAssetError),
    Registry(SimulationInnerRegistryError),
}
struct ProcessedAssets {
    wrappers: Vec<Arc<SimulationInnerRegistryAsset>>,
    plans: Vec<Arc<SimulationNormalArenaPlan>>,
    /// Native `AddSimRegistryAsset` publication precedes asset-list insertion.
    /// Preserve the wrapper even if that later host allocation fails.
    in_progress: Option<Arc<SimulationInnerRegistryAsset>>,
    in_progress_plan: Option<Arc<SimulationNormalArenaPlan>>,
    failed: bool,
}
pub(crate) struct SelectedSimulationAssetServices {
    registry: RegistryBinding,
    processed: Mutex<ProcessedAssets>,
    presentation_navigation: Mutex<presentation_navigation::NavigationRegistration>,
}
impl SelectedSimulationAssetServices {
    pub(crate) fn construct() -> Arc<Self> {
        let (registry, _) = SimulationResidentRegistry::new(std::convert::identity as fn(_) -> _);
        Self::with_binding(RegistryBinding::Standalone(registry))
    }
    fn with_binding(registry: RegistryBinding) -> Arc<Self> {
        Arc::new(Self {
            registry,
            presentation_navigation: Mutex::new(Default::default()),
            processed: Mutex::new(ProcessedAssets {
                wrappers: Vec::new(),
                plans: Vec::new(),
                in_progress: None,
                in_progress_plan: None,
                failed: false,
            }),
        })
    }

    /// Actual selected six ID-only processor subsequences in original plan
    /// order, before `RegArena`. Other processor effects are not certified here.
    /// Each wrapper retains the same input plan Arc, then the outer arena is
    /// published into the SAME namespace after these inner records.
    pub(crate) fn try_process_plan(
        &self,
        plan: &Arc<SimulationNormalArenaPlan>,
    ) -> Result<(), SelectedAssetError> {
        let RegistryBinding::Standalone(registry) = &self.registry else {
            return Err(SelectedAssetError::RequiresResidentPublication);
        };
        self.try_process_plan_into(plan, &mut StandaloneSink { registry, plan })
    }

    fn try_process_plan_into(
        &self,
        plan: &Arc<SimulationNormalArenaPlan>,
        sink: &mut dyn SelectedAssetPublicationSink,
    ) -> Result<(), SelectedAssetError> {
        let mut fields = self
            .processed
            .lock()
            .map_err(|_| SelectedAssetError::Lock)?;
        if fields.failed {
            return Err(SelectedAssetError::PendingPreviousFailure);
        }
        fields.failed = true;
        fields.in_progress_plan = Some(Arc::clone(plan));
        for (index, operation) in plan.operations().iter().enumerate() {
            if !matches!(operation, SimulationNormalOperation::RegistryAsset { .. }) {
                continue;
            }
            let asset = SimulationInnerRegistryAsset::try_from_operation(Arc::clone(plan), index)
                .map_err(SelectedAssetError::Source)?;
            fields.in_progress = Some(Arc::clone(&asset));
            sink.try_add_inner(&asset)?;
            fields
                .wrappers
                .try_reserve(1)
                .map_err(|_| SelectedAssetError::Allocation)?;
            fields.wrappers.push(asset);
            fields.in_progress = None;
        }
        sink.try_add_outer()?;
        fields
            .plans
            .try_reserve(1)
            .map_err(|_| SelectedAssetError::Allocation)?;
        fields.plans.push(Arc::clone(plan));
        fields.in_progress_plan = None;
        fields.failed = false;
        Ok(())
    }

    /// `GetRegAsset` releases the registry lock before the caller reads payload.
    pub(crate) fn try_inner(
        &self,
        id: u64,
    ) -> Result<Option<Arc<SimulationInnerRegistryAsset>>, SelectedAssetError> {
        match &self.registry {
            RegistryBinding::Standalone(registry) => registry.try_inner(CollectionId(id)),
            RegistryBinding::Resident(lookup) => lookup.try_inner(CollectionId(id)),
        }
        .map_err(SelectedAssetError::Registry)
    }

    /// Release the processed owners after the resident registry has removed
    /// this occurrence and its delayed garbage entry has matured. Match the
    /// actual plan allocation, never a GUID that may already have reloaded.
    pub(crate) fn try_release_plan(
        &self,
        plan: &Arc<SimulationNormalArenaPlan>,
    ) -> Result<(), SelectedAssetError> {
        let mut fields = self
            .processed
            .lock()
            .map_err(|_| SelectedAssetError::Lock)?;
        if fields.failed {
            return Err(SelectedAssetError::PendingPreviousFailure);
        }
        fields
            .wrappers
            .retain(|asset| !Arc::ptr_eq(&asset.plan, plan));
        fields.plans.retain(|actual| !Arc::ptr_eq(actual, plan));
        Ok(())
    }

    /// The resident `(plans, wrappers)` counts; the boot probe reports them.
    pub(crate) fn retained_counts(&self) -> (usize, usize) {
        let fields = self.processed.lock().unwrap();
        (fields.plans.len(), fields.wrappers.len())
    }
}
