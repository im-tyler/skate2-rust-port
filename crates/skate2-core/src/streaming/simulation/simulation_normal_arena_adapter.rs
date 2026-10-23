//! Globally ordered normal Simulation arena registration.
//!
//! `Simulation::RegArena` (`0x828a01e0`) and `DRegArena` (`0x828a05f0`)
//! synchronously walk one processed asset-pointer vector forward. This module
//! is the first adapter that preserves the complete reconstructed cross-kind
//! order for kinds 6, 10, 11, 16, 22, 23, 25, 27, 28, and 32.
//!
//! Location kind 21 is a separate post-Simulation arena pass. Kind 31 belongs
//! to a separate collection pass. Native null processed-asset slots and the
//! producer-time `NavMesh` suppression path remain external inputs; this adapter
//! consumes only a successfully prepared dense arena.

use crate::{
    simulation_dmo_assembly_part_session::{
        SimulationDmoBankModelDeregisterController, SimulationDmoBankModelDeregisterPassError,
        SimulationDmoBankModelRegisterController, SimulationDmoBankModelRegisterPassError,
        SimulationDmoRegistrationRuntime,
    },
    simulation_environment_unlock_router::{
        EnvironmentUnlockRouterError, SimulationEnvironmentUnlockRouter,
    },
    simulation_normal_arena_plan::{
        SimulationNormalArenaPlan, SimulationNormalOperation, SimulationRecordHandle,
        SimulationRecordSourceKind, SimulationRegistryAssetKind,
    },
    simulation_pointer_arena_adapter::SimulationPointerArenaDispatch,
    simulation_registry_arena_adapter::SimulationRegistryArenaDispatch,
    simulation_static_arena_adapter::SimulationStaticGrindDispatch,
    simulation_static_arena_plan::SimulationStaticOperation,
    static_ground_pool::{StaticEnvironmentSource, StaticGroundSourceError},
};
use std::{
    convert::Infallible,
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Weak},
};

const DMO_USERDATA_MASK: u32 = 8;

#[path = "simulation_normal_arena_grind_adapter.rs"]
pub(crate) mod grind_runtime;
use grind_runtime::NormalGrindDispatchError;

/// Exact kind-10 manager seam used at its global normal-operation position.
pub trait SimulationNormalEnvironmentDispatch {
    /// Calls the immediate Environment-add route.
    ///
    /// # Errors
    ///
    /// Propagates the concrete router's typed host-capacity error.
    fn register_environment(
        &mut self,
        source: &Arc<StaticEnvironmentSource>,
    ) -> Result<(), EnvironmentUnlockRouterError>;

    fn deregister_environment(&mut self, source: &Arc<StaticEnvironmentSource>);
}

impl SimulationNormalEnvironmentDispatch for SimulationEnvironmentUnlockRouter {
    fn register_environment(
        &mut self,
        source: &Arc<StaticEnvironmentSource>,
    ) -> Result<(), EnvironmentUnlockRouterError> {
        self.register(source)
    }

    fn deregister_environment(&mut self, source: &Arc<StaticEnvironmentSource>) {
        self.deregister(source);
    }
}

/// Complete manager seam used by the reconstructed normal Simulation loop.
pub trait SimulationNormalArenaDispatch:
    SimulationStaticGrindDispatch + SimulationPointerArenaDispatch + SimulationRegistryArenaDispatch
{
}

impl<T> SimulationNormalArenaDispatch for T where
    T: SimulationStaticGrindDispatch
        + SimulationPointerArenaDispatch
        + SimulationRegistryArenaDispatch
{
}

#[derive(Debug)]
enum PreparedNormalOperation {
    Grind {
        grind_asset_index: usize,
    },
    Environment {
        source: Arc<StaticEnvironmentSource>,
    },
    Dmo {
        source: SimulationRecordHandle,
        asset_id: u64,
    },
    Trigger {
        source: SimulationRecordHandle,
    },
    Registry {
        kind: SimulationRegistryAssetKind,
        asset_id: u64,
    },
}

#[derive(Debug)]
struct PreparedSimulationNormalArenaInner {
    plan: Arc<SimulationNormalArenaPlan>,
    operations: Vec<PreparedNormalOperation>,
    has_dmo: bool,
}

/// One fully validated, owner-bound normal Simulation arena.
pub struct PreparedSimulationNormalArena {
    inner: Arc<PreparedSimulationNormalArenaInner>,
}

impl fmt::Debug for PreparedSimulationNormalArena {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSimulationNormalArena")
            .field(
                "asset_id",
                &self.inner.plan.static_plan().asset().decoded.asset_id,
            )
            .field("operation_count", &self.inner.operations.len())
            .field("has_dmo", &self.inner.has_dmo)
            .finish()
    }
}

impl PreparedSimulationNormalArena {
    /// Validates every represented normal operation before manager effects.
    ///
    /// Preparation preserves the plan's exact global order. It validates grind
    /// indexes, constructs every Environment source, and verifies the fixed
    /// kind-11/kind-16 record shapes and both serialized ID lanes.
    ///
    /// # Errors
    ///
    /// Returns a typed source or host-allocation error before runtime effects
    /// exist.
    pub fn try_new(
        plan: Arc<SimulationNormalArenaPlan>,
    ) -> Result<Self, SimulationNormalArenaPrepareError> {
        let requested = plan.operations().len();
        let mut operations = Vec::new();
        operations.try_reserve_exact(requested).map_err(|_| {
            SimulationNormalArenaPrepareError::OperationStorageAllocation { requested }
        })?;
        let mut has_dmo = false;
        for (operation_index, operation) in plan.operations().iter().enumerate() {
            let prepared = prepare_operation(&plan, operation_index, operation, &mut has_dmo)?;
            operations.push(prepared);
        }
        Ok(Self {
            inner: Arc::new(PreparedSimulationNormalArenaInner {
                plan,
                operations,
                has_dmo,
            }),
        })
    }

    #[must_use]
    pub fn plan(&self) -> &SimulationNormalArenaPlan {
        &self.inner.plan
    }

    #[must_use]
    pub(crate) fn plan_owner(&self) -> Arc<SimulationNormalArenaPlan> {
        Arc::clone(&self.inner.plan)
    }

    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.inner.operations.len()
    }
}

fn prepare_operation(
    plan: &SimulationNormalArenaPlan,
    operation_index: usize,
    operation: &SimulationNormalOperation,
    has_dmo: &mut bool,
) -> Result<PreparedNormalOperation, SimulationNormalArenaPrepareError> {
    match *operation {
        SimulationNormalOperation::Grind {
            static_operation_index,
        } => prepare_grind(plan, operation_index, static_operation_index),
        SimulationNormalOperation::Environment {
            static_operation_index,
        } => prepare_environment(plan, operation_index, static_operation_index),
        SimulationNormalOperation::DmoInstance {
            asset_id,
            local_guid,
            ..
        } => {
            *has_dmo = true;
            let source = prepare_record_source(
                plan,
                operation_index,
                SimulationRecordSourceKind::DmoInstance,
                160,
                96,
                asset_id,
                104,
                local_guid,
            )?;
            Ok(PreparedNormalOperation::Dmo { source, asset_id })
        }
        SimulationNormalOperation::TriggerInstance {
            asset_id,
            primary_guid,
            ..
        } => {
            let source = prepare_record_source(
                plan,
                operation_index,
                SimulationRecordSourceKind::TriggerInstance,
                240,
                176,
                primary_guid,
                184,
                asset_id,
            )?;
            Ok(PreparedNormalOperation::Trigger { source })
        }
        SimulationNormalOperation::RegistryAsset { kind, asset_id, .. } => {
            Ok(PreparedNormalOperation::Registry { kind, asset_id })
        }
    }
}

fn prepare_grind(
    plan: &SimulationNormalArenaPlan,
    operation_index: usize,
    static_operation_index: usize,
) -> Result<PreparedNormalOperation, SimulationNormalArenaPrepareError> {
    let static_operation = plan
        .static_plan()
        .operations()
        .get(static_operation_index)
        .ok_or(SimulationNormalArenaPrepareError::MissingStaticOperation {
            operation_index,
            static_operation_index,
        })?;
    let SimulationStaticOperation::Grind {
        grind_asset_index, ..
    } = *static_operation
    else {
        return Err(SimulationNormalArenaPrepareError::WrongStaticOperation {
            operation_index,
            static_operation_index,
            expected: SimulationNormalStaticKind::Grind,
        });
    };
    if plan
        .static_plan()
        .grind_assets()
        .get(grind_asset_index)
        .is_none()
    {
        return Err(SimulationNormalArenaPrepareError::MissingGrindAsset {
            operation_index,
            grind_asset_index,
        });
    }
    Ok(PreparedNormalOperation::Grind { grind_asset_index })
}

fn prepare_environment(
    plan: &SimulationNormalArenaPlan,
    operation_index: usize,
    static_operation_index: usize,
) -> Result<PreparedNormalOperation, SimulationNormalArenaPrepareError> {
    let static_operation = plan
        .static_plan()
        .operations()
        .get(static_operation_index)
        .ok_or(SimulationNormalArenaPrepareError::MissingStaticOperation {
            operation_index,
            static_operation_index,
        })?;
    if !matches!(
        static_operation,
        SimulationStaticOperation::Environment { .. }
    ) {
        return Err(SimulationNormalArenaPrepareError::WrongStaticOperation {
            operation_index,
            static_operation_index,
            expected: SimulationNormalStaticKind::Environment,
        });
    }
    let source =
        StaticEnvironmentSource::from_operation(plan.static_plan_owner(), static_operation_index)
            .map_err(
            |source| SimulationNormalArenaPrepareError::EnvironmentSource {
                operation_index,
                source,
            },
        )?;
    Ok(PreparedNormalOperation::Environment {
        source: Arc::new(source),
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_record_source(
    plan: &SimulationNormalArenaPlan,
    operation_index: usize,
    expected_kind: SimulationRecordSourceKind,
    expected_len: usize,
    first_id_offset: usize,
    first_id: u64,
    second_id_offset: usize,
    second_id: u64,
) -> Result<SimulationRecordHandle, SimulationNormalArenaPrepareError> {
    let source = plan
        .record_handle(operation_index)
        .ok_or(SimulationNormalArenaPrepareError::MissingRecordSource { operation_index })?;
    if source.kind() != expected_kind {
        return Err(SimulationNormalArenaPrepareError::WrongRecordKind {
            operation_index,
            expected: expected_kind,
            actual: source.kind(),
        });
    }
    if source.bytes().len() != expected_len {
        return Err(SimulationNormalArenaPrepareError::WrongRecordLength {
            operation_index,
            expected: expected_len,
            actual: source.bytes().len(),
        });
    }
    if read_u64(source.bytes(), first_id_offset) != Some(first_id)
        || read_u64(source.bytes(), second_id_offset) != Some(second_id)
    {
        return Err(SimulationNormalArenaPrepareError::RecordIdentityMismatch { operation_index });
    }
    Ok(source)
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

#[derive(Debug)]
struct LiveNormalRegistration {
    waypoint_callbacks: bool,
    id: u64,
    prepared: Arc<PreparedSimulationNormalArenaInner>,
    registration_userdata_raw: u32,
    completed_operations: usize,
}

/// Executes one globally ordered normal Simulation operation vector.
///
/// The dispatcher and Environment router precede all source-owning ledgers so
/// they are destroyed first. Tokens and adapter `Drop` never dispatch hidden
/// cleanup. A collaborator panic or Environment routing error latches the exact
/// reached prefix and permanently stops the domain without inventing rollback.
pub struct SimulationNormalArenaRegistrationAdapter<D, E = SimulationEnvironmentUnlockRouter> {
    dispatch: D,
    environment: E,
    live: Vec<LiveNormalRegistration>,
    orphaned_sources: Vec<Arc<PreparedSimulationNormalArenaInner>>,
    domain: Arc<()>,
    next_registration_id: u64,
    fault: Option<SimulationNormalArenaFault>,
    selected_profile: bool,
    waypoint_failure: Option<String>,
}

impl<D: fmt::Debug, E: fmt::Debug> fmt::Debug for SimulationNormalArenaRegistrationAdapter<D, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SimulationNormalArenaRegistrationAdapter")
            .field("dispatch", &self.dispatch)
            .field("environment", &self.environment)
            .field("live_registration_count", &self.live.len())
            .field("orphaned_source_count", &self.orphaned_sources.len())
            .field("next_registration_id", &self.next_registration_id)
            .field("fault", &self.fault)
            .finish_non_exhaustive()
    }
}

impl<D: SimulationNormalArenaDispatch>
    SimulationNormalArenaRegistrationAdapter<D, SimulationEnvironmentUnlockRouter>
{
    #[must_use]
    pub fn new(dispatch: D, environment: SimulationEnvironmentUnlockRouter) -> Self {
        Self::with_environment(dispatch, environment)
    }
}

impl<D: SimulationNormalArenaDispatch, E: SimulationNormalEnvironmentDispatch>
    SimulationNormalArenaRegistrationAdapter<D, E>
{
    #[must_use]
    pub fn with_environment(dispatch: D, environment: E) -> Self {
        Self {
            dispatch,
            environment,
            live: Vec::new(),
            orphaned_sources: Vec::new(),
            domain: Arc::new(()),
            next_registration_id: 0,
            fault: None,
            waypoint_failure: None,
            selected_profile: false,
        }
    }

    /// Explicit host first-test policy; excluded operations are not manager success.
    pub(crate) fn with_selected_profile(dispatch: D, environment: E) -> Self {
        let mut owner = Self::with_environment(dispatch, environment);
        owner.selected_profile = true;
        owner
    }

    /// Exact reached exclusions, retaining each original plan occurrence.
    pub(crate) fn selected_exclusions(
        &self,
    ) -> impl Iterator<Item = (Arc<SimulationNormalArenaPlan>, usize)> + '_ {
        self.live.iter().flat_map(move |row| {
            row.prepared.operations[..row.completed_operations]
                .iter()
                .enumerate()
                .filter(move |(_, operation)| {
                    self.selected_profile
                        && selected_excludes(operation, self.dispatch.selected_rain_enabled())
                })
                .map(move |(index, _)| (Arc::clone(&row.prepared.plan), index))
        })
    }

    /// Registers every prepared operation once in global normal-plan order.
    ///
    /// `userdata_raw` is one arena-call snapshot. Only kind 11 reads bit 3.
    /// The live owner is installed before the first effect.
    ///
    /// # Errors
    ///
    /// Pre-effect host failures have no manager effects. A reached collaborator
    /// panic or Environment error permanently faults the adapter, retains the
    /// complete occurrence owner, and returns no teardown authority.
    pub fn register(
        &mut self,
        prepared: &PreparedSimulationNormalArena,
        userdata_raw: u32,
    ) -> Result<SimulationNormalArenaRegistration, SimulationNormalArenaRegistrationError> {
        match self.register_with_dmo_tail(prepared, userdata_raw, |dispatch, source, asset_id| {
            dispatch.register_dmo_model(source, asset_id);
            Ok::<(), Infallible>(())
        }) {
            Ok(registration) => Ok(registration),
            Err(NormalRegistrationRunError::Arena(source)) => Err(source),
            Err(NormalRegistrationRunError::DmoController { source, .. }) => match source {},
            Err(NormalRegistrationRunError::Grind { fault, .. }) => Err(
                SimulationNormalArenaRegistrationError::OperationFault(fault),
            ),
        }
    }

    /// Registers with the exact kind-11 registry tail supplied by a detached
    /// DMO model controller.
    ///
    /// The native bit-3 gate at `0x828a02e4..0x828a02ec` selects the
    /// `0x828a02f4..0x828a0304` Add setup/call (call at `0x828a0304`), followed
    /// by the DMO-manager virtual dispatch at `0x828a0324`. Controller refusal
    /// is a safe-host fallible override of that native void tail: Add and any
    /// earlier prefix remain applied, the adapter faults, no registration token
    /// is returned, and no rollback or suffix execution is invented.
    #[allow(
        dead_code,
        reason = "staged fallible DMO tail awaits the outer registration coordinator"
    )]
    pub(crate) fn register_with_dmo_model_controller(
        &mut self,
        prepared: &PreparedSimulationNormalArena,
        userdata_raw: u32,
        runtime: &mut SimulationDmoRegistrationRuntime,
        controller: &mut SimulationDmoBankModelRegisterController,
    ) -> Result<SimulationNormalArenaRegistration, SimulationNormalArenaDmoRegistrationError> {
        self.register_with_dmo_tail(prepared, userdata_raw, |_, source, asset_id| {
            runtime.try_model_register_next(controller, source, asset_id)
        })
        .map_err(|source| match source {
            NormalRegistrationRunError::Arena(source) => {
                SimulationNormalArenaDmoRegistrationError::Arena(source)
            }
            NormalRegistrationRunError::DmoController { fault, source } => {
                SimulationNormalArenaDmoRegistrationError::DmoController { fault, source }
            }
            NormalRegistrationRunError::Grind { fault, .. } => {
                SimulationNormalArenaDmoRegistrationError::Arena(
                    SimulationNormalArenaRegistrationError::OperationFault(fault),
                )
            }
        })
    }

    fn register_with_dmo_tail<C, F>(
        &mut self,
        prepared: &PreparedSimulationNormalArena,
        userdata_raw: u32,
        dmo_tail: F,
    ) -> Result<SimulationNormalArenaRegistration, NormalRegistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
    {
        self.register_with_callbacks(
            prepared,
            userdata_raw,
            dmo_tail,
            |dispatch, plan, _, index| {
                dispatch.add_spline_data(&plan.static_plan().grind_assets()[index]);
                Ok(())
            },
        )
    }

    fn register_with_callbacks<C, F, G>(
        &mut self,
        prepared: &PreparedSimulationNormalArena,
        userdata_raw: u32,
        mut dmo_tail: F,
        mut grind: G,
    ) -> Result<SimulationNormalArenaRegistration, NormalRegistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
        G: FnMut(
            &mut D,
            &Arc<SimulationNormalArenaPlan>,
            usize,
            usize,
        ) -> Result<(), NormalGrindDispatchError>,
    {
        self.register_with_callbacks_and_waypoints(prepared, userdata_raw, dmo_tail, grind, None)
    }

    fn register_with_callbacks_and_waypoints<C, F, G>(
        &mut self,
        prepared: &PreparedSimulationNormalArena,
        userdata_raw: u32,
        mut dmo_tail: F,
        mut grind: G,
        mut waypoint: Option<&mut dyn FnMut(u64, bool) -> Result<(), String>>,
    ) -> Result<SimulationNormalArenaRegistration, NormalRegistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
        G: FnMut(
            &mut D,
            &Arc<SimulationNormalArenaPlan>,
            usize,
            usize,
        ) -> Result<(), NormalGrindDispatchError>,
    {
        if let Some(fault) = self.fault {
            return Err(NormalRegistrationRunError::Arena(
                SimulationNormalArenaRegistrationError::AdapterFaulted(fault),
            ));
        }
        self.live.try_reserve(1).map_err(|_| {
            NormalRegistrationRunError::Arena(
                SimulationNormalArenaRegistrationError::LiveStorageAllocation {
                    requested: self.live.len().saturating_add(1),
                },
            )
        })?;
        let registration_id = self.next_registration_id;
        self.next_registration_id = registration_id.checked_add(1).ok_or_else(|| {
            NormalRegistrationRunError::Arena(
                SimulationNormalArenaRegistrationError::RegistrationIdExhausted,
            )
        })?;
        let registration = SimulationNormalArenaRegistration {
            domain: Arc::downgrade(&self.domain),
            id: registration_id,
        };
        self.live.push(LiveNormalRegistration {
            waypoint_callbacks: waypoint.is_some(),
            id: registration_id,
            prepared: Arc::clone(&prepared.inner),
            registration_userdata_raw: userdata_raw,
            completed_operations: 0,
        });

        for (operation_index, operation) in prepared.inner.operations.iter().enumerate() {
            if let PreparedNormalOperation::Registry {kind:SimulationRegistryAssetKind::Waypoint,asset_id}=operation {
                if let Some(callback)=waypoint.as_mut(){
                    let result=catch_unwind(AssertUnwindSafe(||callback(*asset_id, true)));
                    if let Err(message)=result.unwrap_or_else(|_|Err("waypoint callback panicked".into())) {
                        self.waypoint_failure=Some(message.clone());
                        let fault=self.latch_fault(SimulationNormalArenaFaultPhase::Registration,operation_index,
                            SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Waypoint),
                            SimulationNormalArenaFaultCause::WaypointRefusal);
                        return Err(NormalRegistrationRunError::Arena(SimulationNormalArenaRegistrationError::Waypoint {
                            fault, asset_id: *asset_id, detail: message,
                        }));
                    }
                    self.live.last_mut().expect("installed occurrence").completed_operations=operation_index+1;
                    continue;
                }
            }
            if self.selected_profile
                && selected_excludes(operation, self.dispatch.selected_rain_enabled())
            {
                self.live
                    .last_mut()
                    .expect("installed occurrence")
                    .completed_operations = operation_index + 1;
                continue;
            }
            if let Err((stage, cause)) = dispatch_registration_operation(
                &mut self.dispatch,
                &mut self.environment,
                &prepared.inner,
                (operation_index, operation),
                userdata_raw,
                &mut dmo_tail,
                &mut grind,
            ) {
                match cause {
                    NormalOperationRefusal::Arena(cause) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Registration,
                            operation_index,
                            stage,
                            cause,
                        );
                        return Err(NormalRegistrationRunError::Arena(
                            SimulationNormalArenaRegistrationError::OperationFault(fault),
                        ));
                    }
                    NormalOperationRefusal::DmoController(source) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Registration,
                            operation_index,
                            SimulationNormalArenaFaultStage::DmoRegistry,
                            SimulationNormalArenaFaultCause::DmoControllerRefusal,
                        );
                        return Err(NormalRegistrationRunError::DmoController { fault, source });
                    }
                    NormalOperationRefusal::Grind(source) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Registration,
                            operation_index,
                            SimulationNormalArenaFaultStage::Grind,
                            SimulationNormalArenaFaultCause::GrindRefusal,
                        );
                        return Err(NormalRegistrationRunError::Grind { fault, source });
                    }
                }
            }
            self.live
                .last_mut()
                .expect("installed occurrence")
                .completed_operations = operation_index + 1;
        }
        Ok(registration)
    }

    /// Deregisters every prepared operation forward using fresh userdata.
    ///
    /// A set-on-registration/clear-on-deregistration kind-11 asymmetry retains
    /// the complete source owner until adapter destruction because the DMO
    /// manager may still hold its exact record pointer.
    ///
    /// # Errors
    ///
    /// Foreign, unknown, pre-fault, and orphan-allocation failures return the
    /// token unchanged without effects. A reached operation fault consumes the
    /// token, retains the live occurrence, and permanently faults the domain.
    pub fn deregister(
        &mut self,
        registration: SimulationNormalArenaRegistration,
        userdata_raw: u32,
    ) -> Result<(), SimulationNormalArenaDeregistrationError> {
        match self.deregister_with_dmo_tail(
            registration,
            userdata_raw,
            |dispatch, source, asset_id| {
                dispatch.deregister_dmo_model(source, asset_id);
                Ok::<(), Infallible>(())
            },
        ) {
            Ok(()) => Ok(()),
            Err(NormalDeregistrationRunError::Arena(source)) => Err(source),
            Err(NormalDeregistrationRunError::DmoController { source, .. }) => match source {},
            Err(NormalDeregistrationRunError::Grind { fault, .. }) => Err(
                SimulationNormalArenaDeregistrationError::OperationFault(fault),
            ),
        }
    }

    /// Deregisters with the exact kind-11 registry tail supplied by a detached
    /// DMO model controller.
    ///
    /// The native bit-3 gate at `0x828a06f4..0x828a06fc` selects the
    /// `0x828a0704..0x828a0714` Remove setup/call (call at `0x828a0714`),
    /// followed by the DMO-manager virtual dispatch at `0x828a0734`. A
    /// controller refusal is a safe-host override: the token is consumed, the
    /// live occurrence and any applied prefix remain retained, no orphan is
    /// added, and no rollback or suffix execution is invented.
    #[allow(
        dead_code,
        reason = "staged fallible DMO tail awaits the outer registration coordinator"
    )]
    pub(crate) fn deregister_with_dmo_model_controller(
        &mut self,
        registration: SimulationNormalArenaRegistration,
        userdata_raw: u32,
        runtime: &mut SimulationDmoRegistrationRuntime,
        controller: &mut SimulationDmoBankModelDeregisterController,
    ) -> Result<(), SimulationNormalArenaDmoDeregistrationError> {
        self.deregister_with_dmo_tail(registration, userdata_raw, |_, source, asset_id| {
            runtime.try_model_deregister_next(controller, source, asset_id)
        })
        .map_err(|source| match source {
            NormalDeregistrationRunError::Arena(source) => {
                SimulationNormalArenaDmoDeregistrationError::Arena(source)
            }
            NormalDeregistrationRunError::DmoController { fault, source } => {
                SimulationNormalArenaDmoDeregistrationError::DmoController { fault, source }
            }
            NormalDeregistrationRunError::Grind { fault, .. } => {
                SimulationNormalArenaDmoDeregistrationError::Arena(
                    SimulationNormalArenaDeregistrationError::OperationFault(fault),
                )
            }
        })
    }

    fn deregister_with_dmo_tail<C, F>(
        &mut self,
        registration: SimulationNormalArenaRegistration,
        userdata_raw: u32,
        dmo_tail: F,
    ) -> Result<(), NormalDeregistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
    {
        self.deregister_with_callbacks(
            registration,
            userdata_raw,
            dmo_tail,
            |dispatch, plan, _, index| {
                dispatch.remove_spline_data(plan.static_plan().grind_assets()[index].asset_id);
                Ok(())
            },
        )
    }

    fn deregister_with_callbacks<C, F, G>(
        &mut self,
        registration: SimulationNormalArenaRegistration,
        userdata_raw: u32,
        mut dmo_tail: F,
        mut grind: G,
    ) -> Result<(), NormalDeregistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
        G: FnMut(
            &mut D,
            &Arc<SimulationNormalArenaPlan>,
            usize,
            usize,
        ) -> Result<(), NormalGrindDispatchError>,
    {
        self.deregister_with_callbacks_and_waypoints(registration, userdata_raw, dmo_tail, grind, None)
    }

    fn deregister_with_callbacks_and_waypoints<C, F, G>(
        &mut self,
        registration: SimulationNormalArenaRegistration,
        userdata_raw: u32,
        mut dmo_tail: F,
        mut grind: G,
        mut waypoint: Option<&mut dyn FnMut(u64, bool) -> Result<(), String>>,
    ) -> Result<(), NormalDeregistrationRunError<C>>
    where
        F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
        G: FnMut(
            &mut D,
            &Arc<SimulationNormalArenaPlan>,
            usize,
            usize,
        ) -> Result<(), NormalGrindDispatchError>,
    {
        if !Weak::ptr_eq(&registration.domain, &Arc::downgrade(&self.domain)) {
            return Err(NormalDeregistrationRunError::Arena(
                SimulationNormalArenaDeregistrationError::ForeignDomain(registration),
            ));
        }
        if let Some(fault) = self.fault {
            return Err(NormalDeregistrationRunError::Arena(
                SimulationNormalArenaDeregistrationError::AdapterFaulted {
                    fault,
                    registration,
                },
            ));
        }
        let Some(live_index) = self.live.iter().position(|live| live.id == registration.id) else {
            return Err(NormalDeregistrationRunError::Arena(
                SimulationNormalArenaDeregistrationError::UnknownRegistration(registration),
            ));
        };
        let retain_orphan = !self.selected_profile
            && self.live[live_index].prepared.has_dmo
            && dmo_enabled(self.live[live_index].registration_userdata_raw)
            && !dmo_enabled(userdata_raw);
        if retain_orphan {
            self.orphaned_sources.try_reserve(1).map_err(|_| {
                NormalDeregistrationRunError::Arena(
                    SimulationNormalArenaDeregistrationError::OrphanStorageAllocation(registration),
                )
            })?;
        }
        let prepared = Arc::clone(&self.live[live_index].prepared);
        for (operation_index, operation) in prepared.operations.iter().enumerate() {
            if let PreparedNormalOperation::Registry {kind:SimulationRegistryAssetKind::Waypoint,asset_id}=operation {
                if self.live[live_index].waypoint_callbacks && waypoint.is_none() {
                    let detail = "registered waypoint owner missing during deregistration".to_owned();
                    self.waypoint_failure = Some(detail.clone());
                    let fault = self.latch_fault(SimulationNormalArenaFaultPhase::Deregistration, operation_index,
                        SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Waypoint),
                        SimulationNormalArenaFaultCause::WaypointRefusal);
                    return Err(NormalDeregistrationRunError::Arena(SimulationNormalArenaDeregistrationError::Waypoint {
                        fault, asset_id: *asset_id, detail,
                    }));
                }
                if let Some(callback)=waypoint.as_mut(){
                    let result=catch_unwind(AssertUnwindSafe(||callback(*asset_id, false)));
                    if let Err(message)=result.unwrap_or_else(|_|Err("waypoint callback panicked".into())) {
                        self.waypoint_failure=Some(message.clone());
                        let fault=self.latch_fault(SimulationNormalArenaFaultPhase::Deregistration,operation_index,
                            SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Waypoint),
                            SimulationNormalArenaFaultCause::WaypointRefusal);
                        return Err(NormalDeregistrationRunError::Arena(SimulationNormalArenaDeregistrationError::Waypoint {
                            fault, asset_id: *asset_id, detail: message,
                        }));
                    }
                    continue;
                }
            }
            if self.selected_profile
                && selected_excludes(operation, self.dispatch.selected_rain_enabled())
            {
                continue;
            }
            if let Err((stage, cause)) = dispatch_deregistration_operation(
                &mut self.dispatch,
                &mut self.environment,
                &prepared,
                (operation_index, operation),
                userdata_raw,
                &mut dmo_tail,
                &mut grind,
            ) {
                match cause {
                    NormalOperationRefusal::Arena(cause) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Deregistration,
                            operation_index,
                            stage,
                            cause,
                        );
                        return Err(NormalDeregistrationRunError::Arena(
                            SimulationNormalArenaDeregistrationError::OperationFault(fault),
                        ));
                    }
                    NormalOperationRefusal::DmoController(source) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Deregistration,
                            operation_index,
                            SimulationNormalArenaFaultStage::DmoRegistry,
                            SimulationNormalArenaFaultCause::DmoControllerRefusal,
                        );
                        return Err(NormalDeregistrationRunError::DmoController { fault, source });
                    }
                    NormalOperationRefusal::Grind(source) => {
                        let fault = self.latch_fault(
                            SimulationNormalArenaFaultPhase::Deregistration,
                            operation_index,
                            SimulationNormalArenaFaultStage::Grind,
                            SimulationNormalArenaFaultCause::GrindRefusal,
                        );
                        return Err(NormalDeregistrationRunError::Grind { fault, source });
                    }
                }
            }
        }
        if retain_orphan {
            self.orphaned_sources.push(Arc::clone(&prepared));
        }
        self.live.swap_remove(live_index);
        Ok(())
    }

    fn latch_fault(
        &mut self,
        phase: SimulationNormalArenaFaultPhase,
        operation_index: usize,
        stage: SimulationNormalArenaFaultStage,
        cause: SimulationNormalArenaFaultCause,
    ) -> SimulationNormalArenaFault {
        let fault = SimulationNormalArenaFault {
            phase,
            operation_index,
            stage,
            cause,
        };
        self.fault.get_or_insert(fault);
        self.fault.unwrap_or(fault)
    }

    #[must_use]
    pub fn live_registration_count(&self) -> usize {
        self.live.len()
    }

    #[must_use]
    pub fn orphaned_source_count(&self) -> usize {
        self.orphaned_sources.len()
    }

    #[must_use]
    pub const fn fault(&self) -> Option<SimulationNormalArenaFault> {
        self.fault
    }

    #[must_use]
    pub fn waypoint_failure(&self)->Option<&str>{self.waypoint_failure.as_deref()}

    pub const fn dispatch(&self) -> &D {
        &self.dispatch
    }

    #[must_use]
    pub const fn environment(&self) -> &E {
        &self.environment
    }
}

// Host policy only. Required physics/layer/mesh operations retain their order.
fn selected_excludes(operation: &PreparedNormalOperation, rain_enabled: bool) -> bool {
    matches!(
        operation,
        PreparedNormalOperation::Registry {
            kind: SimulationRegistryAssetKind::Rain,
            ..
        }
    ) && !rain_enabled
        || matches!(
            operation,
            PreparedNormalOperation::Dmo { .. }
                | PreparedNormalOperation::Registry {
                    kind: SimulationRegistryAssetKind::Waypoint
                        | SimulationRegistryAssetKind::SpatialMap,
                    ..
                }
        )
}

type OperationFault = (
    SimulationNormalArenaFaultStage,
    SimulationNormalArenaFaultCause,
);

enum NormalOperationRefusal<C> {
    Arena(SimulationNormalArenaFaultCause),
    DmoController(C),
    Grind(NormalGrindDispatchError),
}

#[allow(
    dead_code,
    reason = "controller fault payload is consumed by the staged fallible DMO tail"
)]
enum NormalRegistrationRunError<C> {
    Arena(SimulationNormalArenaRegistrationError),
    DmoController {
        fault: SimulationNormalArenaFault,
        source: C,
    },
    Grind {
        fault: SimulationNormalArenaFault,
        source: NormalGrindDispatchError,
    },
}

#[allow(
    dead_code,
    reason = "controller fault payload is consumed by the staged fallible DMO tail"
)]
enum NormalDeregistrationRunError<C> {
    Arena(SimulationNormalArenaDeregistrationError),
    DmoController {
        fault: SimulationNormalArenaFault,
        source: C,
    },
    Grind {
        fault: SimulationNormalArenaFault,
        source: NormalGrindDispatchError,
    },
}

fn dispatch_registration_operation<
    D: SimulationNormalArenaDispatch,
    E: SimulationNormalEnvironmentDispatch,
    C,
    F,
    G,
>(
    dispatch: &mut D,
    environment: &mut E,
    prepared: &PreparedSimulationNormalArenaInner,
    (operation_index, operation): (usize, &PreparedNormalOperation),
    userdata_raw: u32,
    dmo_tail: &mut F,
    grind: &mut G,
) -> Result<(), (SimulationNormalArenaFaultStage, NormalOperationRefusal<C>)>
where
    F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
    G: FnMut(
        &mut D,
        &Arc<SimulationNormalArenaPlan>,
        usize,
        usize,
    ) -> Result<(), NormalGrindDispatchError>,
{
    match operation {
        PreparedNormalOperation::Grind { grind_asset_index } => dispatch_grind_callback(
            dispatch,
            &prepared.plan,
            operation_index,
            *grind_asset_index,
            grind,
        ),
        PreparedNormalOperation::Environment { source } => {
            match catch_unwind(AssertUnwindSafe(|| {
                environment.register_environment(source)
            })) {
                Ok(Ok(())) => Ok(()),
                Ok(Err(source)) => Err((
                    SimulationNormalArenaFaultStage::Environment,
                    NormalOperationRefusal::Arena(SimulationNormalArenaFaultCause::Environment(
                        source,
                    )),
                )),
                Err(_) => Err((
                    SimulationNormalArenaFaultStage::Environment,
                    NormalOperationRefusal::Arena(SimulationNormalArenaFaultCause::DispatchPanic),
                )),
            }
        }
        PreparedNormalOperation::Dmo { source, asset_id } if dmo_enabled(userdata_raw) => {
            catch_dispatch(SimulationNormalArenaFaultStage::DmoModel, || {
                dispatch.add_dmo_model(*asset_id);
            })
            .map_err(map_operation_fault)?;
            dispatch_dmo_tail(dispatch, source, *asset_id, dmo_tail)
        }
        PreparedNormalOperation::Trigger { source } => {
            catch_dispatch(SimulationNormalArenaFaultStage::Trigger, || {
                dispatch.add_trigger_volume(source);
            })
            .map_err(map_operation_fault)
        }
        PreparedNormalOperation::Registry { kind, asset_id } => {
            dispatch_registry_registration(dispatch, *kind, *asset_id).map_err(map_operation_fault)
        }
        PreparedNormalOperation::Dmo { .. } => Ok(()),
    }
}

fn dispatch_deregistration_operation<
    D: SimulationNormalArenaDispatch,
    E: SimulationNormalEnvironmentDispatch,
    C,
    F,
    G,
>(
    dispatch: &mut D,
    environment: &mut E,
    prepared: &PreparedSimulationNormalArenaInner,
    (operation_index, operation): (usize, &PreparedNormalOperation),
    userdata_raw: u32,
    dmo_tail: &mut F,
    grind: &mut G,
) -> Result<(), (SimulationNormalArenaFaultStage, NormalOperationRefusal<C>)>
where
    F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
    G: FnMut(
        &mut D,
        &Arc<SimulationNormalArenaPlan>,
        usize,
        usize,
    ) -> Result<(), NormalGrindDispatchError>,
{
    match operation {
        PreparedNormalOperation::Grind { grind_asset_index } => dispatch_grind_callback(
            dispatch,
            &prepared.plan,
            operation_index,
            *grind_asset_index,
            grind,
        ),
        PreparedNormalOperation::Environment { source } => {
            catch_dispatch(SimulationNormalArenaFaultStage::Environment, || {
                environment.deregister_environment(source);
            })
            .map_err(map_operation_fault)
        }
        PreparedNormalOperation::Dmo { source, asset_id } if dmo_enabled(userdata_raw) => {
            catch_dispatch(SimulationNormalArenaFaultStage::DmoModel, || {
                dispatch.remove_dmo_model(*asset_id);
            })
            .map_err(map_operation_fault)?;
            dispatch_dmo_tail(dispatch, source, *asset_id, dmo_tail)
        }
        PreparedNormalOperation::Trigger { source } => {
            catch_dispatch(SimulationNormalArenaFaultStage::Trigger, || {
                dispatch.remove_trigger_volume(source);
            })
            .map_err(map_operation_fault)
        }
        PreparedNormalOperation::Registry { kind, asset_id } => {
            dispatch_registry_deregistration(dispatch, *kind, *asset_id)
                .map_err(map_operation_fault)
        }
        PreparedNormalOperation::Dmo { .. } => Ok(()),
    }
}

fn dispatch_grind_callback<D, C, G>(
    dispatch: &mut D,
    plan: &Arc<SimulationNormalArenaPlan>,
    operation_index: usize,
    asset_index: usize,
    grind: &mut G,
) -> Result<(), (SimulationNormalArenaFaultStage, NormalOperationRefusal<C>)>
where
    G: FnMut(
        &mut D,
        &Arc<SimulationNormalArenaPlan>,
        usize,
        usize,
    ) -> Result<(), NormalGrindDispatchError>,
{
    match catch_unwind(AssertUnwindSafe(|| {
        grind(dispatch, plan, operation_index, asset_index)
    })) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err((
            SimulationNormalArenaFaultStage::Grind,
            NormalOperationRefusal::Grind(source),
        )),
        Err(_) => Err((
            SimulationNormalArenaFaultStage::Grind,
            NormalOperationRefusal::Arena(SimulationNormalArenaFaultCause::DispatchPanic),
        )),
    }
}

fn map_operation_fault<C>(
    (stage, cause): OperationFault,
) -> (SimulationNormalArenaFaultStage, NormalOperationRefusal<C>) {
    (stage, NormalOperationRefusal::Arena(cause))
}

fn dispatch_dmo_tail<D, C, F>(
    dispatch: &mut D,
    source: &SimulationRecordHandle,
    asset_id: u64,
    dmo_tail: &mut F,
) -> Result<(), (SimulationNormalArenaFaultStage, NormalOperationRefusal<C>)>
where
    F: FnMut(&mut D, &SimulationRecordHandle, u64) -> Result<(), C>,
{
    match catch_unwind(AssertUnwindSafe(|| dmo_tail(dispatch, source, asset_id))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err((
            SimulationNormalArenaFaultStage::DmoRegistry,
            NormalOperationRefusal::DmoController(source),
        )),
        Err(_) => Err((
            SimulationNormalArenaFaultStage::DmoRegistry,
            NormalOperationRefusal::Arena(SimulationNormalArenaFaultCause::DispatchPanic),
        )),
    }
}

fn catch_dispatch(
    stage: SimulationNormalArenaFaultStage,
    call: impl FnOnce(),
) -> Result<(), OperationFault> {
    catch_unwind(AssertUnwindSafe(call))
        .map_err(|_| (stage, SimulationNormalArenaFaultCause::DispatchPanic))
}

fn dispatch_registry_registration<D: SimulationRegistryArenaDispatch>(
    dispatch: &mut D,
    kind: SimulationRegistryAssetKind,
    asset_id: u64,
) -> Result<(), OperationFault> {
    let stage = SimulationNormalArenaFaultStage::Registry(kind);
    catch_unwind(AssertUnwindSafe(|| {
        match kind {
            SimulationRegistryAssetKind::WorldPainter => {
                return dispatch
                    .try_register_streamed_tile(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::WorldPainter);
            }
            SimulationRegistryAssetKind::NavMesh => {
                return dispatch
                    .try_add_nav_mesh_data(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::NavMesh);
            }
            SimulationRegistryAssetKind::Rain => {
                return dispatch
                    .try_add_rain_data(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::Rain);
            }
            SimulationRegistryAssetKind::AiPath => dispatch.add_ai_path_asset(asset_id),
            SimulationRegistryAssetKind::Waypoint => {
                dispatch.register_waypoint_data(asset_id, true);
            }
            SimulationRegistryAssetKind::SpatialMap => dispatch.add_spatial_map_asset(asset_id),
        }
        Ok(())
    }))
    .map_err(|_| (stage, SimulationNormalArenaFaultCause::DispatchPanic))?
    .map_err(|source| (stage, source))
}

fn dispatch_registry_deregistration<D: SimulationRegistryArenaDispatch>(
    dispatch: &mut D,
    kind: SimulationRegistryAssetKind,
    asset_id: u64,
) -> Result<(), OperationFault> {
    let stage = SimulationNormalArenaFaultStage::Registry(kind);
    catch_unwind(AssertUnwindSafe(|| {
        match kind {
            SimulationRegistryAssetKind::WorldPainter => {
                return dispatch
                    .try_unregister_streamed_tile(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::WorldPainter);
            }
            SimulationRegistryAssetKind::NavMesh => {
                return dispatch
                    .try_remove_nav_mesh_data(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::NavMesh);
            }
            SimulationRegistryAssetKind::Rain => {
                return dispatch
                    .try_remove_rain_data(asset_id)
                    .map_err(SimulationNormalArenaFaultCause::Rain);
            }
            SimulationRegistryAssetKind::AiPath => dispatch.remove_ai_path_asset(asset_id),
            SimulationRegistryAssetKind::Waypoint => {
                dispatch.deregister_waypoint_data(asset_id, false);
            }
            SimulationRegistryAssetKind::SpatialMap => {
                dispatch.remove_spatial_map_asset(asset_id);
            }
        }
        Ok(())
    }))
    .map_err(|_| (stage, SimulationNormalArenaFaultCause::DispatchPanic))?
    .map_err(|source| (stage, source))
}

const fn dmo_enabled(userdata_raw: u32) -> bool {
    userdata_raw & DMO_USERDATA_MASK == DMO_USERDATA_MASK
}

/// Non-cloneable authority for one complete normal-arena occurrence.
pub struct SimulationNormalArenaRegistration {
    domain: Weak<()>,
    id: u64,
}

impl fmt::Debug for SimulationNormalArenaRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SimulationNormalArenaRegistration")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationNormalStaticKind {
    Grind,
    Environment,
}

#[derive(Debug)]
pub enum SimulationNormalArenaPrepareError {
    OperationStorageAllocation {
        requested: usize,
    },
    MissingStaticOperation {
        operation_index: usize,
        static_operation_index: usize,
    },
    WrongStaticOperation {
        operation_index: usize,
        static_operation_index: usize,
        expected: SimulationNormalStaticKind,
    },
    MissingGrindAsset {
        operation_index: usize,
        grind_asset_index: usize,
    },
    EnvironmentSource {
        operation_index: usize,
        source: StaticGroundSourceError,
    },
    MissingRecordSource {
        operation_index: usize,
    },
    WrongRecordKind {
        operation_index: usize,
        expected: SimulationRecordSourceKind,
        actual: SimulationRecordSourceKind,
    },
    WrongRecordLength {
        operation_index: usize,
        expected: usize,
        actual: usize,
    },
    RecordIdentityMismatch {
        operation_index: usize,
    },
}

impl fmt::Display for SimulationNormalArenaPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationStorageAllocation { requested } => write!(
                formatter,
                "could not allocate {requested} globally ordered Simulation operations"
            ),
            Self::MissingStaticOperation {
                operation_index,
                static_operation_index,
            } => write!(
                formatter,
                "normal operation {operation_index} references missing static operation {static_operation_index}"
            ),
            Self::WrongStaticOperation {
                operation_index,
                static_operation_index,
                expected,
            } => write!(
                formatter,
                "normal operation {operation_index} references static operation {static_operation_index} with the wrong kind; expected {expected:?}"
            ),
            Self::MissingGrindAsset {
                operation_index,
                grind_asset_index,
            } => write!(
                formatter,
                "normal operation {operation_index} references missing grind asset {grind_asset_index}"
            ),
            Self::EnvironmentSource {
                operation_index,
                source,
            } => write!(
                formatter,
                "normal Environment operation {operation_index} is invalid: {source}"
            ),
            Self::MissingRecordSource { operation_index } => write!(
                formatter,
                "normal pointer operation {operation_index} has no retained record source"
            ),
            Self::WrongRecordKind {
                operation_index,
                expected,
                actual,
            } => write!(
                formatter,
                "normal pointer operation {operation_index} has record kind {actual:?}, expected {expected:?}"
            ),
            Self::WrongRecordLength {
                operation_index,
                expected,
                actual,
            } => write!(
                formatter,
                "normal pointer operation {operation_index} has {actual} bytes, expected {expected}"
            ),
            Self::RecordIdentityMismatch { operation_index } => write!(
                formatter,
                "normal pointer operation {operation_index} disagrees with its retained record IDs"
            ),
        }
    }
}

impl Error for SimulationNormalArenaPrepareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EnvironmentSource { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulationNormalArenaFault {
    phase: SimulationNormalArenaFaultPhase,
    operation_index: usize,
    stage: SimulationNormalArenaFaultStage,
    cause: SimulationNormalArenaFaultCause,
}

impl SimulationNormalArenaFault {
    #[must_use]
    pub const fn phase(self) -> SimulationNormalArenaFaultPhase {
        self.phase
    }

    #[must_use]
    pub const fn operation_index(self) -> usize {
        self.operation_index
    }

    #[must_use]
    pub const fn stage(self) -> SimulationNormalArenaFaultStage {
        self.stage
    }

    #[must_use]
    pub const fn cause(self) -> SimulationNormalArenaFaultCause {
        self.cause
    }

    fn error_source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.cause {
            SimulationNormalArenaFaultCause::Environment(source) => Some(source),
            SimulationNormalArenaFaultCause::NavMesh(source) => Some(source),
            SimulationNormalArenaFaultCause::Rain(source) => Some(source),
            SimulationNormalArenaFaultCause::WorldPainter(source) => Some(source),
            SimulationNormalArenaFaultCause::SelectedTriggerPending
            | SimulationNormalArenaFaultCause::DispatchPanic
            | SimulationNormalArenaFaultCause::GrindRefusal
            | SimulationNormalArenaFaultCause::WaypointRefusal
            | SimulationNormalArenaFaultCause::DmoControllerRefusal => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationNormalArenaFaultPhase {
    Registration,
    Deregistration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationNormalArenaFaultStage {
    Grind,
    Environment,
    DmoModel,
    DmoRegistry,
    Trigger,
    Registry(SimulationRegistryAssetKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationNormalArenaFaultCause {
    /// Selected host profile cannot claim native trigger-manager registration.
    SelectedTriggerPending,
    DispatchPanic,
    Environment(EnvironmentUnlockRouterError),
    NavMesh(crate::simulation_registry_arena_adapter::MeshStoreError),
    Rain(crate::simulation_registry_arena_adapter::RainNetworkError),
    WorldPainter(crate::simulation_registry_arena_adapter::WorldPainterLayerError),
    DmoControllerRefusal,
    GrindRefusal,
    WaypointRefusal,
}

#[derive(Debug)]
pub enum SimulationNormalArenaRegistrationError {
    AdapterFaulted(SimulationNormalArenaFault),
    LiveStorageAllocation { requested: usize },
    RegistrationIdExhausted,
    OperationFault(SimulationNormalArenaFault),
    /// Host callback detail must survive both Display (replay) and Debug
    /// (live terrain) wrappers; the latched native operation is unchanged.
    Waypoint { fault: SimulationNormalArenaFault, asset_id: u64, detail: String },
}

impl fmt::Display for SimulationNormalArenaRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdapterFaulted(_) => formatter.write_str("normal Simulation adapter is faulted"),
            Self::LiveStorageAllocation { requested } => write!(
                formatter,
                "could not allocate normal Simulation registration {requested}"
            ),
            Self::RegistrationIdExhausted => {
                formatter.write_str("normal Simulation registration IDs are exhausted")
            }
            Self::OperationFault(fault) => write!(
                formatter,
                "normal Simulation registration faulted at operation {} stage {:?}: {:?}",
                fault.operation_index, fault.stage, fault.cause
            ),
            Self::Waypoint { fault, asset_id, detail } => write!(
                formatter,
                "normal Simulation waypoint registration faulted at operation {} asset {asset_id:#018x}: {detail}",
                fault.operation_index
            ),
        }
    }
}

impl Error for SimulationNormalArenaRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AdapterFaulted(fault) | Self::OperationFault(fault)
            | Self::Waypoint { fault, .. } => fault.error_source(),
            Self::LiveStorageAllocation { .. } | Self::RegistrationIdExhausted => None,
        }
    }
}

/// Crate-private typed refusal from registration with a DMO model controller.
#[allow(
    dead_code,
    reason = "staged fallible DMO tail awaits the outer registration coordinator"
)]
#[derive(Debug)]
pub(crate) enum SimulationNormalArenaDmoRegistrationError {
    Arena(SimulationNormalArenaRegistrationError),
    DmoController {
        fault: SimulationNormalArenaFault,
        source: SimulationDmoBankModelRegisterPassError,
    },
}

impl fmt::Display for SimulationNormalArenaDmoRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arena(source) => source.fmt(formatter),
            Self::DmoController { fault, .. } => write!(
                formatter,
                "normal Simulation registration DMO controller refused at operation {}",
                fault.operation_index
            ),
        }
    }
}

impl Error for SimulationNormalArenaDmoRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Arena(source) => Some(source),
            Self::DmoController { source, .. } => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum SimulationNormalArenaDeregistrationError {
    ForeignDomain(SimulationNormalArenaRegistration),
    UnknownRegistration(SimulationNormalArenaRegistration),
    AdapterFaulted {
        fault: SimulationNormalArenaFault,
        registration: SimulationNormalArenaRegistration,
    },
    OrphanStorageAllocation(SimulationNormalArenaRegistration),
    OperationFault(SimulationNormalArenaFault),
    Waypoint { fault: SimulationNormalArenaFault, asset_id: u64, detail: String },
}

impl SimulationNormalArenaDeregistrationError {
    #[must_use]
    pub fn into_registration(self) -> Option<SimulationNormalArenaRegistration> {
        match self {
            Self::ForeignDomain(registration)
            | Self::UnknownRegistration(registration)
            | Self::OrphanStorageAllocation(registration)
            | Self::AdapterFaulted { registration, .. } => Some(registration),
            Self::OperationFault(_) | Self::Waypoint { .. } => None,
        }
    }
}

impl fmt::Display for SimulationNormalArenaDeregistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Waypoint { fault, asset_id, detail } => write!(
                formatter,
                "normal Simulation waypoint deregistration faulted at operation {} asset {asset_id:#018x}: {detail}",
                fault.operation_index
            ),
            Self::ForeignDomain(_) => formatter.write_str("registration belongs to another normal Simulation adapter"),
            Self::UnknownRegistration(_) => formatter.write_str("normal Simulation registration is not live"),
            Self::AdapterFaulted { .. } => formatter.write_str("normal Simulation adapter is faulted"),
            Self::OrphanStorageAllocation(_) => formatter.write_str(
                "could not retain an asymmetrically deregistered normal Simulation source"
            ),
            Self::OperationFault(_) => formatter.write_str("normal Simulation deregistration collaborator faulted"),
        }
    }
}

impl Error for SimulationNormalArenaDeregistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AdapterFaulted { fault, .. } | Self::OperationFault(fault)
            | Self::Waypoint { fault, .. } => {
                fault.error_source()
            }
            Self::ForeignDomain(_)
            | Self::UnknownRegistration(_)
            | Self::OrphanStorageAllocation(_) => None,
        }
    }
}

/// Crate-private typed refusal from deregistration with a DMO model controller.
#[allow(
    dead_code,
    reason = "staged fallible DMO tail awaits the outer registration coordinator"
)]
#[derive(Debug)]
pub(crate) enum SimulationNormalArenaDmoDeregistrationError {
    Arena(SimulationNormalArenaDeregistrationError),
    DmoController {
        fault: SimulationNormalArenaFault,
        source: SimulationDmoBankModelDeregisterPassError,
    },
}

impl SimulationNormalArenaDmoDeregistrationError {
    /// Recovers authority only from the same pre-effect errors as the legacy
    /// path. A reached controller refusal has consumed the token.
    #[allow(
        dead_code,
        reason = "staged fallible DMO tail awaits the outer registration coordinator"
    )]
    pub(crate) fn into_registration(self) -> Option<SimulationNormalArenaRegistration> {
        match self {
            Self::Arena(source) => source.into_registration(),
            Self::DmoController { .. } => None,
        }
    }
}

impl fmt::Display for SimulationNormalArenaDmoDeregistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arena(source) => source.fmt(formatter),
            Self::DmoController { fault, .. } => write!(
                formatter,
                "normal Simulation deregistration DMO controller refused at operation {}",
                fault.operation_index
            ),
        }
    }
}

impl Error for SimulationNormalArenaDmoDeregistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Arena(source) => Some(source),
            Self::DmoController { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    mod grind_runtime_tests {
        include!("simulation_normal_arena_grind_adapter_tests.rs");
    }
    use super::*;
    use crate::{
        collection_load_pump::LoadedAssetCollection,
        collection_resident_streamer::ResidentCollectionStorage,
        decoded_arena_asset::{DecodedArenaAsset, WORLD_STREAM_ARENA_ASSET_TYPE},
        dmo_bank_processed_asset::DmoBankProcessedArenaAsset,
        grind::GrindAsset,
        owned_resident_collection::{
            OwnedResidentCollection, OwnedResidentCollectionError, OwnedResidentCollectionStorage,
        },
        simulation_collision_asset::SimulationCollisionAsset,
        simulation_dmo_assembly_part_session::{
            SimulationDmoBankModelRegistrationBatch, SimulationDmoBankModelRegistrationPassError,
        },
        simulation_environment_unlock_router::{
            EnvironmentUnlockContextSeed, EnvironmentUnlockSeed,
        },
        simulation_normal_arena_plan::{
            AI_PATH_DATA_TYPE_ID, NAV_MESH_2_DATA_TYPE_ID, RAIN_DATA_TYPE_ID,
            SPATIAL_MAP_DATA_TYPE_ID, WAYPOINT_DATA_TYPE_ID, WORLD_PAINTER_DATA_TYPE_ID_0F,
        },
        simulation_static_arena_plan::SimulationStaticArenaPlan,
        world::{CollisionArena, CollisionInstance, CollisionMesh, CollisionModel, CollisionRoot},
    };
    use skate2_formats::{
        collision::{ClusteredMesh, Volume},
        pegasus::{
            INSTANCE_DATA_TYPE_ID, INSTANCE_DMO_ASSET_TYPE_ID, ObjectId, SPLINE_DATA_TYPE_ID,
            SplineData, TABLE_OF_CONTENTS_TYPE_ID, TRIGGER_INSTANCE_DATA_TYPE_ID,
        },
        rw4::{Arena, DictionaryEntry, FileHeader, ResourceDescriptor},
        stream::ArenaResources,
    };
    use std::{
        ops::Range,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    const ENVIRONMENT_TYPE_ID: u32 = 0xacb3_1c9a;
    const ENVIRONMENT_LOCAL_GUID: u64 = 0xeeee_0000_0000_0001;

    type DmoTestStorage = OwnedResidentCollectionStorage<
        DmoBankProcessedArenaAsset,
        fn(OwnedResidentCollectionError) -> OwnedResidentCollectionError,
        OwnedResidentCollectionError,
    >;

    fn identity_collection_error(
        error: OwnedResidentCollectionError,
    ) -> OwnedResidentCollectionError {
        error
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Event {
        Grind(u64, bool),
        DmoModel(u64, bool),
        DmoRegistry(u64, bool),
        Trigger(u64, bool),
        Registry(SimulationRegistryAssetKind, u64, bool, Option<bool>),
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PanicCall {
        Grind,
        DmoModel,
        DmoRegistry,
        Trigger,
        Registry(SimulationRegistryAssetKind),
    }

    #[derive(Default)]
    struct RecorderState {
        events: Vec<Event>,
        panic_on: Option<PanicCall>,
        mesh: Option<Arc<crate::living_world_mesh_store::MeshStore>>,
    }

    #[derive(Clone, Default)]
    struct Recorder {
        state: Arc<Mutex<RecorderState>>,
    }

    impl fmt::Debug for Recorder {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let state = self.state.lock().unwrap();
            formatter
                .debug_struct("Recorder")
                .field("events", &state.events)
                .field("panic_on", &state.panic_on)
                .finish()
        }
    }

    impl Recorder {
        fn record(&self, event: Event, call: PanicCall) {
            let should_panic = {
                let mut state = self.state.lock().unwrap();
                state.events.push(event);
                state.panic_on == Some(call)
            };
            assert!(!should_panic, "requested recorder panic");
        }

        fn set_panic(&self, call: Option<PanicCall>) {
            self.state.lock().unwrap().panic_on = call;
        }

        fn events(&self) -> Vec<Event> {
            self.state.lock().unwrap().events.clone()
        }

        fn clear_events(&self) {
            self.state.lock().unwrap().events.clear();
        }
    }

    impl SimulationStaticGrindDispatch for Recorder {
        fn add_spline_data(&mut self, asset: &GrindAsset) {
            self.record(Event::Grind(asset.asset_id, true), PanicCall::Grind);
        }

        fn remove_spline_data(&mut self, asset_id: u64) {
            self.record(Event::Grind(asset_id, false), PanicCall::Grind);
        }
    }

    impl SimulationPointerArenaDispatch for Recorder {
        fn add_dmo_model(&mut self, asset_id: u64) {
            self.record(Event::DmoModel(asset_id, true), PanicCall::DmoModel);
        }

        fn register_dmo_model(&mut self, _source: &SimulationRecordHandle, asset_id: u64) {
            self.record(Event::DmoRegistry(asset_id, true), PanicCall::DmoRegistry);
        }

        fn remove_dmo_model(&mut self, asset_id: u64) {
            self.record(Event::DmoModel(asset_id, false), PanicCall::DmoModel);
        }

        fn deregister_dmo_model(&mut self, _source: &SimulationRecordHandle, asset_id: u64) {
            self.record(Event::DmoRegistry(asset_id, false), PanicCall::DmoRegistry);
        }

        fn add_trigger_volume(&mut self, source: &SimulationRecordHandle) {
            self.record(
                Event::Trigger(read_record_u64(source, 184), true),
                PanicCall::Trigger,
            );
        }

        fn remove_trigger_volume(&mut self, source: &SimulationRecordHandle) {
            self.record(
                Event::Trigger(read_record_u64(source, 184), false),
                PanicCall::Trigger,
            );
        }
    }

    impl SimulationRegistryArenaDispatch for Recorder {
        fn register_streamed_tile(&mut self, asset_id: u64) {
            self.registry(
                SimulationRegistryAssetKind::WorldPainter,
                asset_id,
                true,
                None,
            );
        }

        fn unregister_streamed_tile(&mut self, asset_id: u64) {
            self.registry(
                SimulationRegistryAssetKind::WorldPainter,
                asset_id,
                false,
                None,
            );
        }

        fn add_nav_mesh_data(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::NavMesh, asset_id, true, None);
        }

        fn remove_nav_mesh_data(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::NavMesh, asset_id, false, None);
        }

        fn try_add_nav_mesh_data(
            &mut self,
            asset_id: u64,
        ) -> Result<(), crate::living_world_mesh_store::MeshStoreError> {
            let mesh = self.state.lock().unwrap().mesh.clone();
            if let Some(mesh) = mesh {
                return mesh.try_add_tile(asset_id);
            }
            self.add_nav_mesh_data(asset_id);
            Ok(())
        }
        fn try_remove_nav_mesh_data(
            &mut self,
            asset_id: u64,
        ) -> Result<(), crate::living_world_mesh_store::MeshStoreError> {
            let mesh = self.state.lock().unwrap().mesh.clone();
            if let Some(mesh) = mesh {
                return mesh.try_remove_tile(asset_id);
            }
            self.remove_nav_mesh_data(asset_id);
            Ok(())
        }
        fn add_rain_data(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::Rain, asset_id, true, None);
        }

        fn remove_rain_data(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::Rain, asset_id, false, None);
        }

        fn add_ai_path_asset(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::AiPath, asset_id, true, None);
        }

        fn remove_ai_path_asset(&mut self, asset_id: u64) {
            self.registry(SimulationRegistryAssetKind::AiPath, asset_id, false, None);
        }

        fn register_waypoint_data(&mut self, asset_id: u64, streamed: bool) {
            self.registry(
                SimulationRegistryAssetKind::Waypoint,
                asset_id,
                true,
                Some(streamed),
            );
        }

        fn deregister_waypoint_data(&mut self, asset_id: u64, streamed: bool) {
            self.registry(
                SimulationRegistryAssetKind::Waypoint,
                asset_id,
                false,
                Some(streamed),
            );
        }

        fn add_spatial_map_asset(&mut self, asset_id: u64) {
            self.registry(
                SimulationRegistryAssetKind::SpatialMap,
                asset_id,
                true,
                None,
            );
        }

        fn remove_spatial_map_asset(&mut self, asset_id: u64) {
            self.registry(
                SimulationRegistryAssetKind::SpatialMap,
                asset_id,
                false,
                None,
            );
        }
    }

    impl Recorder {
        fn registry(
            &self,
            kind: SimulationRegistryAssetKind,
            asset_id: u64,
            registration: bool,
            waypoint: Option<bool>,
        ) {
            self.record(
                Event::Registry(kind, asset_id, registration, waypoint),
                PanicCall::Registry(kind),
            );
        }
    }

    fn read_record_u64(source: &SimulationRecordHandle, offset: usize) -> u64 {
        u64::from_be_bytes(source.bytes()[offset..offset + 8].try_into().unwrap())
    }

    struct ArenaFixture {
        bytes: Vec<u8>,
        dictionary: Vec<DictionaryEntry>,
    }

    impl ArenaFixture {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                dictionary: Vec::new(),
            }
        }

        fn push(&mut self, type_id: u32, object: &[u8]) -> ObjectId {
            let index = self.dictionary.len();
            let start = self.bytes.len();
            self.bytes.extend_from_slice(object);
            let end = self.bytes.len();
            self.dictionary.push(DictionaryEntry {
                resource_index: 0,
                object_offset: u32::try_from(start).unwrap(),
                relocation_offset: 0,
                size: u32::try_from(object.len()).unwrap(),
                alignment: 1,
                type_index: 0,
                type_id,
                object_range: Range { start, end },
            });
            ObjectId(u32::try_from(index).unwrap())
        }

        fn finish(
            self,
            static_operations: Vec<SimulationStaticOperation>,
            grind_assets: Vec<GrindAsset>,
        ) -> Arc<SimulationNormalArenaPlan> {
            let descriptor = ResourceDescriptor {
                size: 0,
                alignment: 1,
            };
            let entry_count = u32::try_from(self.dictionary.len()).unwrap();
            let virtual_size = u32::try_from(self.bytes.len()).unwrap();
            let arena = Arena {
                file_header: FileHeader {
                    magic: *b"\x89RW4xb2\0\r\n\x1a\n",
                    is_big_endian: true,
                    pointer_size_bits: 32,
                    pointer_alignment: 4,
                    unused: 0,
                    major_version: *b"454\0",
                    minor_version: *b"000\0",
                    build_number: 0,
                },
                id: 0x1234_5678,
                entry_count,
                used_entry_count: entry_count,
                alignment: 1,
                virtual_size,
                dictionary_offset: 0,
                sections_offset: 0,
                base_offset: 0,
                unfix_context_offset: 0,
                fix_context_offset: 0,
                resource_descriptor: [descriptor; 5],
                resources_used: [descriptor; 5],
                resource_offsets: [0; 5],
                arena_group_offset: 0,
                dictionary: self.dictionary,
            };
            let volume = Volume {
                transform: [0.0; 16],
                vtable_offset: 0,
                shape_words: [0; 3],
                radius: 0.0,
                group_id: 0,
                surface_id: u32::MAX,
                flags: 1,
            };
            let collision = CollisionArena {
                id: 0x1234_5678,
                grind_assets: Vec::new(),
                aggregate_roots: vec![CollisionRoot {
                    dictionary_index: 0,
                    volume,
                    aggregate: Some(crate::world::CollisionAggregateRef::Mesh(0)),
                }],
                primitive_sets: Vec::new(),
                simple_primitive_sets: Vec::new(),
                meshes: vec![CollisionMesh {
                    dictionary_index: 0,
                    bounding_box_min: [0.0; 3],
                    bounding_box_max: [0.0; 3],
                    triangles: Vec::new(),
                    clustered_mesh: ClusteredMesh::parse(&[0; 96]).unwrap(),
                    object_bytes: Vec::new(),
                }],
                collision_models: vec![CollisionModel {
                    dictionary_index: 0,
                    bounding_root: None,
                    collision_roots: vec![0],
                    assembly_object_id: None,
                }],
                instances: vec![CollisionInstance {
                    guid: 0xdddd_0000_0000_0001,
                    local_guid: ENVIRONMENT_LOCAL_GUID,
                    name: "environment".to_owned(),
                    transform: [0.0; 16],
                    bounding_box_min: [0.0; 3],
                    bounding_box_max: [1.0; 3],
                    collision_model: Some(0),
                }],
            };
            let static_plan = Arc::new(SimulationStaticArenaPlan::from_parts_with_grind_for_test(
                SimulationCollisionAsset {
                    decoded: DecodedArenaAsset::from_parts_for_test(
                        0xabcd_ef01_2345_6789,
                        WORLD_STREAM_ARENA_ASSET_TYPE,
                        ArenaResources {
                            resources: [self.bytes, Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                        },
                        arena,
                    ),
                    collision,
                },
                grind_assets,
                static_operations,
            ));
            Arc::new(SimulationNormalArenaPlan::from_static_plan(static_plan).unwrap())
        }
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    fn toc(entries: &[(u64, u32, ObjectId)]) -> Vec<u8> {
        let names = 20 + entries.len() * 24;
        let mut bytes = vec![0; names + 1];
        put_u32(&mut bytes, 0, u32::try_from(entries.len()).unwrap());
        put_u32(&mut bytes, 4, 20);
        put_u32(&mut bytes, 8, u32::try_from(names).unwrap());
        put_u32(&mut bytes, 16, u32::try_from(names).unwrap());
        for (index, (guid, object_type, object)) in entries.iter().enumerate() {
            let offset = 20 + index * 24;
            put_u32(&mut bytes, offset, u32::try_from(names).unwrap());
            put_u64(&mut bytes, offset + 8, *guid);
            put_u32(&mut bytes, offset + 16, *object_type);
            put_u32(&mut bytes, offset + 20, object.0);
        }
        bytes
    }

    fn instance_data(subtype: u32, records: &[(u64, u64)]) -> Vec<u8> {
        let mut bytes = vec![0; 20 + records.len() * 160];
        put_u32(&mut bytes, 0, subtype);
        put_u32(&mut bytes, 4, u32::try_from(records.len()).unwrap());
        put_u32(&mut bytes, 12, 20);
        let string_list_offset = u32::try_from(bytes.len()).unwrap();
        put_u32(&mut bytes, 16, string_list_offset);
        for (index, &(primary, local)) in records.iter().enumerate() {
            let start = 20 + index * 160;
            put_u64(&mut bytes, start + 96, primary);
            put_u64(&mut bytes, start + 104, local);
        }
        bytes
    }

    fn trigger_data(primary: u64, local: u64) -> Vec<u8> {
        let mut bytes = vec![0; 20 + 240];
        put_u32(&mut bytes, 0, 0xdead_beef);
        put_u32(&mut bytes, 4, 1);
        put_u32(&mut bytes, 12, 20);
        let string_list_offset = u32::try_from(bytes.len()).unwrap();
        put_u32(&mut bytes, 16, string_list_offset);
        put_u64(&mut bytes, 20 + 176, primary);
        put_u64(&mut bytes, 20 + 184, local);
        bytes
    }

    fn mixed_plan() -> Arc<SimulationNormalArenaPlan> {
        let mut fixture = ArenaFixture::new();
        let world_painter = fixture.push(WORLD_PAINTER_DATA_TYPE_ID_0F, &[0]);
        let spline = fixture.push(SPLINE_DATA_TYPE_ID, &[0]);
        let rain = fixture.push(RAIN_DATA_TYPE_ID, &[0]);
        let environment = fixture.push(
            INSTANCE_DATA_TYPE_ID,
            &instance_data(ENVIRONMENT_TYPE_ID, &[(0x10, ENVIRONMENT_LOCAL_GUID)]),
        );
        let ai_path = fixture.push(AI_PATH_DATA_TYPE_ID, &[0]);
        let dmo = fixture.push(
            INSTANCE_DATA_TYPE_ID,
            &instance_data(INSTANCE_DMO_ASSET_TYPE_ID, &[(0x60, 0x61)]),
        );
        let waypoint = fixture.push(WAYPOINT_DATA_TYPE_ID, &[0]);
        let trigger = fixture.push(TRIGGER_INSTANCE_DATA_TYPE_ID, &trigger_data(0x70, 0x71));
        let spatial = fixture.push(SPATIAL_MAP_DATA_TYPE_ID, &[0]);
        let nav = fixture.push(NAV_MESH_2_DATA_TYPE_ID, &[0]);
        fixture.push(
            TABLE_OF_CONTENTS_TYPE_ID,
            &toc(&[
                (
                    0x0000_0001_0000_0042,
                    WORLD_PAINTER_DATA_TYPE_ID_0F,
                    world_painter,
                ),
                (0x20, SPLINE_DATA_TYPE_ID, spline),
                (0x30, RAIN_DATA_TYPE_ID, rain),
                (0x40, INSTANCE_DATA_TYPE_ID, environment),
                (0x50, AI_PATH_DATA_TYPE_ID, ai_path),
                (0x51, INSTANCE_DATA_TYPE_ID, dmo),
                (0x80, WAYPOINT_DATA_TYPE_ID, waypoint),
                (0x81, TRIGGER_INSTANCE_DATA_TYPE_ID, trigger),
                (0x90, SPATIAL_MAP_DATA_TYPE_ID, spatial),
                (0xffff_ffff_0000_0042, NAV_MESH_2_DATA_TYPE_ID, nav),
            ]),
        );
        let grind = GrindAsset::from_spline_data(
            0x20,
            spline.entry_index(),
            &SplineData {
                splines_offset: 0,
                segments_offset: 0,
                splines: Vec::new(),
                segments: Vec::new(),
            },
        );
        fixture.finish(
            vec![
                SimulationStaticOperation::Grind {
                    source_toc_index: 1,
                    source_dictionary_index: spline.entry_index(),
                    grind_asset_index: 0,
                },
                SimulationStaticOperation::Environment {
                    source_toc_index: 3,
                    source_dictionary_index: environment.entry_index(),
                    source_inline_index: 0,
                    source_resource_index: 0,
                    source_record_start: 20,
                    instance_index: 0,
                },
            ],
            vec![grind],
        )
    }

    fn model_batch_for_plans(
        plans: &[Arc<SimulationNormalArenaPlan>],
    ) -> (
        DmoTestStorage,
        OwnedResidentCollection<DmoBankProcessedArenaAsset>,
        SimulationDmoBankModelRegistrationBatch,
    ) {
        let mut storage = DmoTestStorage::new(identity_collection_error);
        let collection = storage
            .create_empty(i32::try_from(plans.len()).unwrap())
            .unwrap();
        let mut writer = storage.writer(&collection).unwrap();
        for plan in plans {
            writer.add_asset(DmoBankProcessedArenaAsset::from_parts_for_test(
                Arc::clone(plan),
                Vec::new(),
            ));
        }
        drop(writer);
        let batch = SimulationDmoBankModelRegistrationBatch::try_new(&collection).unwrap();
        (storage, collection, batch)
    }

    fn empty_plan() -> Arc<SimulationNormalArenaPlan> {
        ArenaFixture::new().finish(Vec::new(), Vec::new())
    }

    fn router(capacity: usize) -> SimulationEnvironmentUnlockRouter {
        SimulationEnvironmentUnlockRouter::try_new_empty(capacity).unwrap()
    }

    fn expected_events() -> Vec<Event> {
        vec![
            Event::Registry(
                SimulationRegistryAssetKind::WorldPainter,
                0x0000_0001_0000_0042,
                true,
                None,
            ),
            Event::Grind(0x20, true),
            Event::Registry(SimulationRegistryAssetKind::Rain, 0x30, true, None),
            Event::Registry(SimulationRegistryAssetKind::AiPath, 0x50, true, None),
            Event::DmoModel(0x60, true),
            Event::DmoRegistry(0x60, true),
            Event::Registry(
                SimulationRegistryAssetKind::Waypoint,
                0x80,
                true,
                Some(true),
            ),
            Event::Trigger(0x71, true),
            Event::Registry(SimulationRegistryAssetKind::SpatialMap, 0x90, true, None),
            Event::Registry(
                SimulationRegistryAssetKind::NavMesh,
                0xffff_ffff_0000_0042,
                true,
                None,
            ),
            Event::Registry(
                SimulationRegistryAssetKind::WorldPainter,
                0x0000_0001_0000_0042,
                false,
                None,
            ),
            Event::Grind(0x20, false),
            Event::Registry(SimulationRegistryAssetKind::Rain, 0x30, false, None),
            Event::Registry(SimulationRegistryAssetKind::AiPath, 0x50, false, None),
            Event::DmoModel(0x60, false),
            Event::DmoRegistry(0x60, false),
            Event::Registry(
                SimulationRegistryAssetKind::Waypoint,
                0x80,
                false,
                Some(false),
            ),
            Event::Trigger(0x71, false),
            Event::Registry(SimulationRegistryAssetKind::SpatialMap, 0x90, false, None),
            Event::Registry(
                SimulationRegistryAssetKind::NavMesh,
                0xffff_ffff_0000_0042,
                false,
                None,
            ),
        ]
    }

    #[test]
    fn selected_profile_removal_never_calls_excluded_dmo_or_retains_false_orphan() {
        let mut fixture = ArenaFixture::new();
        let layer = fixture.push(WORLD_PAINTER_DATA_TYPE_ID_0F, &[0]);
        let dmo = fixture.push(
            INSTANCE_DATA_TYPE_ID,
            &instance_data(INSTANCE_DMO_ASSET_TYPE_ID, &[(0x60, 0x61)]),
        );
        let rain = fixture.push(RAIN_DATA_TYPE_ID, &[0]);
        let nav = fixture.push(NAV_MESH_2_DATA_TYPE_ID, &[0]);
        fixture.push(
            TABLE_OF_CONTENTS_TYPE_ID,
            &toc(&[
                (0x10, WORLD_PAINTER_DATA_TYPE_ID_0F, layer),
                (0x20, INSTANCE_DATA_TYPE_ID, dmo),
                (0x30, RAIN_DATA_TYPE_ID, rain),
                (0x40, NAV_MESH_2_DATA_TYPE_ID, nav),
            ]),
        );
        let plan = fixture.finish(Vec::new(), Vec::new());
        let prepared = PreparedSimulationNormalArena::try_new(plan).unwrap();
        let recorder = Recorder::default();
        let mut adapter = SimulationNormalArenaRegistrationAdapter::with_selected_profile(
            recorder.clone(),
            router(4),
        );
        let token = adapter.register(&prepared, 8).unwrap();
        assert_eq!(
            adapter
                .selected_exclusions()
                .map(|(_, index)| index)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        adapter.deregister(token, 0).unwrap();
        assert_eq!(adapter.orphaned_source_count(), 0);
        assert_eq!(adapter.live_registration_count(), 0);
        assert_eq!(
            recorder.events(),
            vec![
                Event::Registry(SimulationRegistryAssetKind::WorldPainter, 0x10, true, None),
                Event::Registry(SimulationRegistryAssetKind::NavMesh, 0x40, true, None),
                Event::Registry(SimulationRegistryAssetKind::WorldPainter, 0x10, false, None),
                Event::Registry(SimulationRegistryAssetKind::NavMesh, 0x40, false, None),
            ]
        );
    }

    #[test]
    fn selected_profile_retains_exclusions_and_dispatches_trigger() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let recorder = Recorder::default();
        let mut adapter = SimulationNormalArenaRegistrationAdapter::with_selected_profile(
            recorder.clone(),
            router(4),
        );
        adapter.register(&prepared, 8).unwrap();
        assert!(adapter.fault().is_none());
        let expected = expected_events();
        assert_eq!(
            recorder.events(),
            // The selected profile dispatches AI paths to its real PathManager
            // rather than excluding them, so 3 registers like every other
            // live kind.
            vec![
                expected[0].clone(),
                expected[1].clone(),
                expected[3].clone(),
                expected[7].clone(),
                expected[9].clone()
            ]
        );
        let excluded: Vec<_> = adapter
            .selected_exclusions()
            .map(|(actual, index)| {
                assert!(Arc::ptr_eq(&actual, &plan));
                index
            })
            .collect();
        assert_eq!(excluded, [2, 5, 6, 8]);
        assert_eq!(adapter.live_registration_count(), 1);
    }

    #[test]
    fn typed_mesh_refusal_preserves_exact_normal_registration_prefix() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        let recorder = Recorder::default();
        recorder.state.lock().unwrap().mesh =
            Some(crate::living_world_mesh_store::MeshStore::construct());
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        assert!(matches!(
            adapter.register(&prepared, 0x8000_0008),
            Err(SimulationNormalArenaRegistrationError::OperationFault(_))
        ));
        let fault = adapter.fault().unwrap();
        assert_eq!(fault.operation_index(), 9);
        assert_eq!(
            fault.stage(),
            SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::NavMesh)
        );
        assert_eq!(
            fault.cause(),
            SimulationNormalArenaFaultCause::NavMesh(
                crate::living_world_mesh_store::MeshStoreError::PendingTileAdmission {
                    asset_id: 0xffff_ffff_0000_0042
                }
            )
        );
        assert_eq!(recorder.events(), expected_events()[..9]);
        assert_eq!(adapter.environment().published_volume_count(), 1);
        assert!(matches!(
            adapter.register(&prepared, 0x8000_0008),
            Err(SimulationNormalArenaRegistrationError::AdapterFaulted(_))
        ));
        assert_eq!(recorder.events(), expected_events()[..9]);
    }

    #[test]
    fn typed_mesh_removal_refusal_preserves_forward_teardown_prefix() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let registration = adapter.register(&prepared, 0x8000_0008).unwrap();
        recorder.state.lock().unwrap().mesh =
            Some(crate::living_world_mesh_store::MeshStore::construct());
        assert!(adapter.deregister(registration, 8).is_err());
        let fault = adapter.fault().unwrap();
        assert_eq!(
            fault.phase(),
            SimulationNormalArenaFaultPhase::Deregistration
        );
        assert_eq!(fault.operation_index(), 9);
        assert_eq!(
            fault.cause(),
            SimulationNormalArenaFaultCause::NavMesh(
                crate::living_world_mesh_store::MeshStoreError::PendingTileRemoval {
                    asset_id: 0xffff_ffff_0000_0042
                }
            )
        );
        assert_eq!(recorder.events(), expected_events()[..19]);
        assert_eq!(adapter.environment().published_volume_count(), 0);
    }

    #[test]
    fn globally_interleaved_registration_and_forward_teardown_are_exact() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        assert_eq!(prepared.operation_count(), 10);
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));

        let registration = adapter.register(&prepared, 0x8000_0008).unwrap();
        assert_eq!(adapter.environment().published_volume_count(), 1);
        adapter.deregister(registration, 8).unwrap();

        assert_eq!(recorder.events(), expected_events());
        assert_eq!(adapter.environment().published_volume_count(), 0);
        assert_eq!(adapter.live_registration_count(), 0);
        assert_eq!(adapter.orphaned_source_count(), 0);
        assert_eq!(adapter.fault(), None);
    }

    #[test]
    fn registration_and_deregistration_userdata_are_independent() {
        for (registration_userdata, deregistration_userdata, adds, removes, orphaned) in [
            (0, 0, 0, 0, 0),
            (0, 8, 0, 2, 0),
            (8, 0, 2, 0, 1),
            (u32::MAX, 8, 2, 2, 0),
        ] {
            let plan = mixed_plan();
            let weak = Arc::downgrade(&plan);
            let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
            let recorder = Recorder::default();
            let mut adapter =
                SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
            let registration = adapter.register(&prepared, registration_userdata).unwrap();
            adapter
                .deregister(registration, deregistration_userdata)
                .unwrap();
            let events = recorder.events();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        Event::DmoModel(_, true) | Event::DmoRegistry(_, true)
                    ))
                    .count(),
                adds
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        Event::DmoModel(_, false) | Event::DmoRegistry(_, false)
                    ))
                    .count(),
                removes
            );
            assert_eq!(adapter.orphaned_source_count(), orphaned);
            drop(prepared);
            drop(plan);
            assert_eq!(weak.upgrade().is_some(), orphaned != 0);
            drop(adapter);
            assert!(weak.upgrade().is_none());
        }
    }

    #[test]
    fn controlled_dmo_tail_preserves_mixed_order_without_legacy_callbacks() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut register = runtime.begin_model_register_controller(&batch);

        let token = adapter
            .register_with_dmo_model_controller(&prepared, 8, &mut runtime, &mut register)
            .unwrap();
        register.finish().unwrap();
        assert!(runtime.has_model_for_test(0x61));
        let mut deregister = runtime.begin_model_deregister_controller(&batch);
        adapter
            .deregister_with_dmo_model_controller(token, 8, &mut runtime, &mut deregister)
            .unwrap();
        deregister.finish().unwrap();
        assert!(!runtime.has_model_for_test(0x61));

        let expected = expected_events()
            .into_iter()
            .filter(|event| !matches!(event, Event::DmoRegistry(..)))
            .collect::<Vec<_>>();
        assert_eq!(recorder.events(), expected);
    }

    #[test]
    fn add_panic_prevents_controller_advance() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let recorder = Recorder::default();
        recorder.set_panic(Some(PanicCall::DmoModel));
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut controller = runtime.begin_model_register_controller(&batch);

        let error = adapter
            .register_with_dmo_model_controller(&prepared, 8, &mut runtime, &mut controller)
            .unwrap_err();
        assert!(matches!(
            error,
            SimulationNormalArenaDmoRegistrationError::Arena(
                SimulationNormalArenaRegistrationError::OperationFault(fault)
            ) if fault.stage() == SimulationNormalArenaFaultStage::DmoModel
        ));
        assert!(matches!(
            controller.finish(),
            Err(SimulationDmoBankModelRegisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::Incomplete {
                    issued: 0,
                    expected: 1
                }
            ))
        ));
        assert!(!runtime.has_model_for_test(0x61));
    }

    #[test]
    fn source_refusal_occurs_after_add_and_stops_suffix() {
        let actual_plan = mixed_plan();
        let expected_plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&actual_plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[expected_plan]);
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut controller = runtime.begin_model_register_controller(&batch);

        let error = adapter
            .register_with_dmo_model_controller(&prepared, 8, &mut runtime, &mut controller)
            .unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        let SimulationNormalArenaDmoRegistrationError::DmoController { fault, source } = error
        else {
            panic!("wrong error")
        };
        assert_eq!(fault.phase(), SimulationNormalArenaFaultPhase::Registration);
        assert_eq!(fault.operation_index(), 5);
        assert_eq!(fault.stage(), SimulationNormalArenaFaultStage::DmoRegistry);
        assert_eq!(
            fault.cause(),
            SimulationNormalArenaFaultCause::DmoControllerRefusal
        );
        assert!(matches!(
            source,
            SimulationDmoBankModelRegisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::SourceMismatch { ordinal: 0 }
            )
        ));
        assert_eq!(recorder.events(), &expected_events()[..5]);
        assert_eq!(adapter.live_registration_count(), 1);
        assert!(!runtime.has_model_for_test(0x61));
    }

    #[test]
    fn wrong_runtime_does_not_latch_controller_but_faults_adapter() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let mut owner = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut foreign = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut controller = owner.begin_model_register_controller(&batch);
        let mut refused =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));

        let error = refused
            .register_with_dmo_model_controller(&prepared, 8, &mut foreign, &mut controller)
            .unwrap_err();
        assert!(matches!(
            error,
            SimulationNormalArenaDmoRegistrationError::DmoController {
                source: SimulationDmoBankModelRegisterPassError::RuntimeMismatch,
                ..
            }
        ));
        assert_eq!(
            refused.fault().unwrap().cause(),
            SimulationNormalArenaFaultCause::DmoControllerRefusal
        );
        assert!(!foreign.has_model_for_test(0x61));

        let mut accepted =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));
        accepted
            .register_with_dmo_model_controller(&prepared, 8, &mut owner, &mut controller)
            .unwrap();
        controller.finish().unwrap();
        assert!(owner.has_model_for_test(0x61));
    }

    #[test]
    fn earlier_arena_model_prefix_survives_later_source_failure() {
        let first_plan = mixed_plan();
        let expected_second = mixed_plan();
        let wrong_second = mixed_plan();
        let first = PreparedSimulationNormalArena::try_new(Arc::clone(&first_plan)).unwrap();
        let second = PreparedSimulationNormalArena::try_new(wrong_second).unwrap();
        let (_storage, _collection, batch) =
            model_batch_for_plans(&[Arc::clone(&first_plan), expected_second]);
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut controller = runtime.begin_model_register_controller(&batch);
        let mut first_adapter =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));
        let mut second_adapter =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));

        first_adapter
            .register_with_dmo_model_controller(&first, 8, &mut runtime, &mut controller)
            .unwrap();
        assert!(runtime.has_model_for_test(0x61));
        assert!(matches!(
            second_adapter.register_with_dmo_model_controller(
                &second,
                8,
                &mut runtime,
                &mut controller,
            ),
            Err(SimulationNormalArenaDmoRegistrationError::DmoController { .. })
        ));
        assert!(runtime.has_model_for_test(0x61));
        assert!(matches!(
            controller.finish(),
            Err(SimulationDmoBankModelRegisterPassError::AlreadyFaulted)
        ));
    }

    #[test]
    fn deregistration_refusal_consumes_token_and_retains_live_model_owner() {
        let plan = mixed_plan();
        let weak = Arc::downgrade(&plan);
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let mut owner = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut foreign = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let mut register = owner.begin_model_register_controller(&batch);
        let token = adapter
            .register_with_dmo_model_controller(&prepared, 8, &mut owner, &mut register)
            .unwrap();
        register.finish().unwrap();
        let mut deregister = owner.begin_model_deregister_controller(&batch);
        recorder.clear_events();

        let error = adapter
            .deregister_with_dmo_model_controller(token, 8, &mut foreign, &mut deregister)
            .unwrap_err();
        let fault = match &error {
            SimulationNormalArenaDmoDeregistrationError::DmoController {
                fault,
                source: SimulationDmoBankModelDeregisterPassError::RuntimeMismatch,
            } => *fault,
            _ => panic!("wrong error"),
        };
        assert_eq!(
            fault.phase(),
            SimulationNormalArenaFaultPhase::Deregistration
        );
        assert_eq!(fault.operation_index(), 5);
        assert_eq!(fault.stage(), SimulationNormalArenaFaultStage::DmoRegistry);
        assert_eq!(
            fault.cause(),
            SimulationNormalArenaFaultCause::DmoControllerRefusal
        );
        assert_eq!(adapter.fault(), Some(fault));
        assert!(error.into_registration().is_none());
        assert_eq!(adapter.live_registration_count(), 1);
        assert_eq!(adapter.orphaned_source_count(), 0);
        assert_eq!(recorder.events(), &expected_events()[10..15]);
        assert!(owner.has_model_for_test(0x61));
        drop(deregister);
        drop(batch);
        drop(collection);
        drop(prepared);
        drop(plan);
        assert!(weak.upgrade().is_some());
    }

    #[test]
    fn clear_bit_skips_controller_and_preserves_asymmetric_orphan_rule() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let recorder = Recorder::default();
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        let mut skipped_register = runtime.begin_model_register_controller(&batch);
        let token = adapter
            .register_with_dmo_model_controller(&prepared, 0, &mut runtime, &mut skipped_register)
            .unwrap();
        assert!(matches!(
            skipped_register.finish(),
            Err(SimulationDmoBankModelRegisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::Incomplete { issued: 0, .. }
            ))
        ));
        let mut skipped_deregister = runtime.begin_model_deregister_controller(&batch);
        adapter
            .deregister_with_dmo_model_controller(token, 0, &mut runtime, &mut skipped_deregister)
            .unwrap();
        assert!(matches!(
            skipped_deregister.finish(),
            Err(SimulationDmoBankModelDeregisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::Incomplete { issued: 0, .. }
            ))
        ));
        assert!(
            !recorder
                .events()
                .iter()
                .any(|event| matches!(event, Event::DmoModel(..) | Event::DmoRegistry(..)))
        );

        let mut enabled = runtime.begin_model_register_controller(&batch);
        let token = adapter
            .register_with_dmo_model_controller(&prepared, 8, &mut runtime, &mut enabled)
            .unwrap();
        enabled.finish().unwrap();
        let mut disabled = runtime.begin_model_deregister_controller(&batch);
        adapter
            .deregister_with_dmo_model_controller(token, 0, &mut runtime, &mut disabled)
            .unwrap();
        assert!(matches!(
            disabled.finish(),
            Err(SimulationDmoBankModelDeregisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::Incomplete { issued: 0, .. }
            ))
        ));
        assert_eq!(adapter.orphaned_source_count(), 1);
        assert!(runtime.has_model_for_test(0x61));
    }

    #[test]
    fn controlled_deregister_preserves_pre_effect_token_authority() {
        let plan = mixed_plan();
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let (_storage, _collection, batch) = model_batch_for_plans(&[Arc::clone(&plan)]);
        let mut owner =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));
        let mut foreign =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(4));
        let token = owner.register(&prepared, 0).unwrap();
        let mut runtime = SimulationDmoRegistrationRuntime::try_new().unwrap();
        let mut controller = runtime.begin_model_deregister_controller(&batch);

        let error = foreign
            .deregister_with_dmo_model_controller(token, 8, &mut runtime, &mut controller)
            .unwrap_err();
        assert!(matches!(
            &error,
            SimulationNormalArenaDmoDeregistrationError::Arena(
                SimulationNormalArenaDeregistrationError::ForeignDomain(_)
            )
        ));
        let token = error.into_registration().unwrap();
        assert!(matches!(
            controller.finish(),
            Err(SimulationDmoBankModelDeregisterPassError::Source(
                SimulationDmoBankModelRegistrationPassError::Incomplete { issued: 0, .. }
            ))
        ));
        owner.deregister(token, 0).unwrap();
    }

    #[test]
    fn empty_and_repeated_occurrences_have_explicit_authority() {
        let prepared = PreparedSimulationNormalArena::try_new(empty_plan()).unwrap();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(0));
        let first = adapter.register(&prepared, 1).unwrap();
        let second = adapter.register(&prepared, 2).unwrap();
        assert_eq!(adapter.live_registration_count(), 2);
        adapter.deregister(second, 3).unwrap();
        adapter.deregister(first, 4).unwrap();
        assert_eq!(adapter.live_registration_count(), 0);
    }

    #[test]
    fn foreign_token_is_returned_without_effects() {
        let prepared = PreparedSimulationNormalArena::try_new(empty_plan()).unwrap();
        let mut first =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(0));
        let mut second =
            SimulationNormalArenaRegistrationAdapter::new(Recorder::default(), router(0));
        let registration = first.register(&prepared, 0).unwrap();

        let error = second.deregister(registration, 0).unwrap_err();
        assert!(matches!(
            error,
            SimulationNormalArenaDeregistrationError::ForeignDomain(_)
        ));
        assert_eq!(second.live_registration_count(), 0);
        first
            .deregister(error.into_registration().unwrap(), 0)
            .unwrap();
    }

    #[test]
    fn every_dispatch_stage_latches_its_exact_registration_prefix() {
        let cases = [
            (
                PanicCall::Registry(SimulationRegistryAssetKind::WorldPainter),
                0,
                SimulationNormalArenaFaultStage::Registry(
                    SimulationRegistryAssetKind::WorldPainter,
                ),
                1,
            ),
            (
                PanicCall::Grind,
                1,
                SimulationNormalArenaFaultStage::Grind,
                2,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::Rain),
                2,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Rain),
                3,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::AiPath),
                4,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::AiPath),
                4,
            ),
            (
                PanicCall::DmoModel,
                5,
                SimulationNormalArenaFaultStage::DmoModel,
                5,
            ),
            (
                PanicCall::DmoRegistry,
                5,
                SimulationNormalArenaFaultStage::DmoRegistry,
                6,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::Waypoint),
                6,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Waypoint),
                7,
            ),
            (
                PanicCall::Trigger,
                7,
                SimulationNormalArenaFaultStage::Trigger,
                8,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::SpatialMap),
                8,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::SpatialMap),
                9,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::NavMesh),
                9,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::NavMesh),
                10,
            ),
        ];
        for (panic_call, operation_index, stage, event_count) in cases {
            let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
            let recorder = Recorder::default();
            recorder.set_panic(Some(panic_call));
            let mut adapter =
                SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));

            let error = adapter.register(&prepared, 8).unwrap_err();
            let SimulationNormalArenaRegistrationError::OperationFault(fault) = error else {
                panic!("wrong error: {error:?}");
            };
            assert_eq!(fault.phase(), SimulationNormalArenaFaultPhase::Registration);
            assert_eq!(fault.operation_index(), operation_index);
            assert_eq!(fault.stage(), stage);
            assert_eq!(
                fault.cause(),
                SimulationNormalArenaFaultCause::DispatchPanic
            );
            assert_eq!(recorder.events(), expected_events()[..event_count]);
            assert_eq!(adapter.live_registration_count(), 1);
            assert!(matches!(
                adapter.register(&prepared, 8),
                Err(SimulationNormalArenaRegistrationError::AdapterFaulted(replayed)) if replayed == fault
            ));
        }
    }

    #[test]
    fn every_dispatch_stage_latches_its_exact_deregistration_prefix() {
        let cases = [
            (
                PanicCall::Registry(SimulationRegistryAssetKind::WorldPainter),
                0,
                SimulationNormalArenaFaultStage::Registry(
                    SimulationRegistryAssetKind::WorldPainter,
                ),
                1,
            ),
            (
                PanicCall::Grind,
                1,
                SimulationNormalArenaFaultStage::Grind,
                2,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::Rain),
                2,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Rain),
                3,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::AiPath),
                4,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::AiPath),
                4,
            ),
            (
                PanicCall::DmoModel,
                5,
                SimulationNormalArenaFaultStage::DmoModel,
                5,
            ),
            (
                PanicCall::DmoRegistry,
                5,
                SimulationNormalArenaFaultStage::DmoRegistry,
                6,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::Waypoint),
                6,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::Waypoint),
                7,
            ),
            (
                PanicCall::Trigger,
                7,
                SimulationNormalArenaFaultStage::Trigger,
                8,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::SpatialMap),
                8,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::SpatialMap),
                9,
            ),
            (
                PanicCall::Registry(SimulationRegistryAssetKind::NavMesh),
                9,
                SimulationNormalArenaFaultStage::Registry(SimulationRegistryAssetKind::NavMesh),
                10,
            ),
        ];
        for (panic_call, operation_index, stage, event_count) in cases {
            let plan = mixed_plan();
            let weak = Arc::downgrade(&plan);
            let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
            let recorder = Recorder::default();
            let mut adapter =
                SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
            let registration = adapter.register(&prepared, 8).unwrap();
            recorder.clear_events();
            recorder.set_panic(Some(panic_call));

            let error = adapter.deregister(registration, 8).unwrap_err();
            let SimulationNormalArenaDeregistrationError::OperationFault(fault) = error else {
                panic!("wrong error: {error:?}");
            };
            assert_eq!(
                fault.phase(),
                SimulationNormalArenaFaultPhase::Deregistration
            );
            assert_eq!(fault.operation_index(), operation_index);
            assert_eq!(fault.stage(), stage);
            assert_eq!(recorder.events(), expected_events()[10..10 + event_count]);
            assert_eq!(adapter.live_registration_count(), 1);
            drop(prepared);
            drop(plan);
            assert!(weak.upgrade().is_some());
        }
    }

    #[test]
    fn exhausted_registration_id_has_no_effects() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(4));
        adapter.next_registration_id = u64::MAX;

        assert!(matches!(
            adapter.register(&prepared, 8),
            Err(SimulationNormalArenaRegistrationError::RegistrationIdExhausted)
        ));
        assert!(recorder.events().is_empty());
        assert_eq!(adapter.live_registration_count(), 0);
        assert_eq!(adapter.environment().published_volume_count(), 0);
    }

    #[test]
    fn prior_token_is_returned_without_effects_after_a_later_fault() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), router(8));
        let first = adapter.register(&prepared, 8).unwrap();
        recorder.set_panic(Some(PanicCall::Registry(
            SimulationRegistryAssetKind::WorldPainter,
        )));
        assert!(matches!(
            adapter.register(&prepared, 8),
            Err(SimulationNormalArenaRegistrationError::OperationFault(_))
        ));
        recorder.set_panic(None);
        let event_count = recorder.events().len();

        let error = adapter.deregister(first, 8).unwrap_err();
        assert!(matches!(
            error,
            SimulationNormalArenaDeregistrationError::AdapterFaulted { .. }
        ));
        assert_eq!(recorder.events().len(), event_count);
        assert!(error.into_registration().is_some());
        assert_eq!(adapter.live_registration_count(), 2);
    }

    #[test]
    fn environment_capacity_fault_keeps_the_native_store_prefix() {
        let prepared = PreparedSimulationNormalArena::try_new(mixed_plan()).unwrap();
        let environment = SimulationEnvironmentUnlockRouter::try_new_with_external_seed(
            EnvironmentUnlockSeed {
                locations_lock_status_raw: 0,
                contexts_in_native_iteration_order: vec![EnvironmentUnlockContextSeed {
                    local_guid: ENVIRONMENT_LOCAL_GUID,
                    challenge_index: -1,
                    is_unlocked_env_instance_raw: 1,
                }],
            },
            0,
        )
        .unwrap();
        let recorder = Recorder::default();
        let mut adapter =
            SimulationNormalArenaRegistrationAdapter::new(recorder.clone(), environment);

        let error = adapter.register(&prepared, 8).unwrap_err();
        assert!(std::error::Error::source(&error).is_some());
        let SimulationNormalArenaRegistrationError::OperationFault(fault) = error else {
            panic!("wrong error: {error:?}");
        };
        assert_eq!(fault.operation_index(), 3);
        assert_eq!(fault.stage(), SimulationNormalArenaFaultStage::Environment);
        assert!(matches!(
            fault.cause(),
            SimulationNormalArenaFaultCause::Environment(
                EnvironmentUnlockRouterError::GroundCapacity(_)
            )
        ));
        assert_eq!(
            recorder.events(),
            [
                Event::Registry(
                    SimulationRegistryAssetKind::WorldPainter,
                    0x0000_0001_0000_0042,
                    true,
                    None,
                ),
                Event::Grind(0x20, true),
                Event::Registry(SimulationRegistryAssetKind::Rain, 0x30, true, None),
            ]
        );
        let context = adapter.environment().context_snapshots()[0];
        assert!(context.has_attached_source);
        assert!(context.is_in_world);
        assert_eq!(adapter.live_registration_count(), 1);
    }

    #[test]
    fn dropped_token_is_inert_and_dispatch_drops_before_retained_owner() {
        struct DropObserver {
            plan: Weak<SimulationNormalArenaPlan>,
            source_was_alive: Arc<AtomicBool>,
        }

        impl Drop for DropObserver {
            fn drop(&mut self) {
                self.source_was_alive
                    .store(self.plan.upgrade().is_some(), Ordering::Release);
            }
        }

        impl SimulationStaticGrindDispatch for DropObserver {
            fn add_spline_data(&mut self, _asset: &GrindAsset) {}
            fn remove_spline_data(&mut self, _asset_id: u64) {}
        }

        impl SimulationPointerArenaDispatch for DropObserver {
            fn add_dmo_model(&mut self, _asset_id: u64) {}
            fn register_dmo_model(&mut self, _source: &SimulationRecordHandle, _asset_id: u64) {}
            fn remove_dmo_model(&mut self, _asset_id: u64) {}
            fn deregister_dmo_model(&mut self, _source: &SimulationRecordHandle, _asset_id: u64) {}
            fn add_trigger_volume(&mut self, _source: &SimulationRecordHandle) {}
            fn remove_trigger_volume(&mut self, _source: &SimulationRecordHandle) {}
        }

        impl SimulationRegistryArenaDispatch for DropObserver {
            fn register_streamed_tile(&mut self, _asset_id: u64) {}
            fn unregister_streamed_tile(&mut self, _asset_id: u64) {}
            fn add_nav_mesh_data(&mut self, _asset_id: u64) {}
            fn remove_nav_mesh_data(&mut self, _asset_id: u64) {}
            fn add_rain_data(&mut self, _asset_id: u64) {}
            fn remove_rain_data(&mut self, _asset_id: u64) {}
            fn add_ai_path_asset(&mut self, _asset_id: u64) {}
            fn remove_ai_path_asset(&mut self, _asset_id: u64) {}
            fn register_waypoint_data(&mut self, _asset_id: u64, _streamed: bool) {}
            fn deregister_waypoint_data(&mut self, _asset_id: u64, _streamed: bool) {}
            fn add_spatial_map_asset(&mut self, _asset_id: u64) {}
            fn remove_spatial_map_asset(&mut self, _asset_id: u64) {}
        }

        let plan = empty_plan();
        let weak = Arc::downgrade(&plan);
        let prepared = PreparedSimulationNormalArena::try_new(Arc::clone(&plan)).unwrap();
        let source_was_alive = Arc::new(AtomicBool::new(false));
        let dispatch = DropObserver {
            plan: weak.clone(),
            source_was_alive: Arc::clone(&source_was_alive),
        };
        let mut adapter = SimulationNormalArenaRegistrationAdapter::new(dispatch, router(0));
        let registration = adapter.register(&prepared, 0).unwrap();
        drop(registration);
        drop(prepared);
        drop(plan);
        assert!(weak.upgrade().is_some());

        drop(adapter);

        assert!(source_was_alive.load(Ordering::Acquire));
        assert!(weak.upgrade().is_none());
    }
}
