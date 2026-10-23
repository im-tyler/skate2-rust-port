//! Structural fixture tests, UNRUN. No claim of original-model gameplay parity.
use super::*;
use crate::{
    living_world_grind_services::character_models::collision_assembly::tests::{
        create, definition,
    },
    serialised_assembly_bbox::HostSerialisedAssemblyBBoxNumeric,
};

#[test]
fn source_living_world_body_view_keeps_entity_body_and_assembly_ids_distinct() {
    let (mut backend, mut manager, source) = super::super::tests::fixture(4, 1);
    let mut pool = SimulationDmoAssemblyPool::try_new(&manager, &backend, false).unwrap();
    let candidate = SimulationDmoAssemblyCandidate::try_from_living_world(
        &source.parts,
        701,
        crate::skateboard_wheels::FRESH_VOLUME_TRANSFORM,
        14,
        source.parts.template().culling_table().unwrap().clone(),
    )
    .unwrap();
    let body = pool
        .try_insert(
            &mut manager,
            &mut backend,
            candidate,
            4,
            &mut HostSerialisedAssemblyBBoxNumeric,
        )
        .unwrap();
    let assembly_id = pool.candidate(&body).unwrap().world_index();
    pool.try_set_island_body_id(&backend, &body, 91).unwrap();
    let view = pool.island_body(&body).unwrap();
    assert_eq!(view.entity_id, Some(701));
    assert_eq!(view.body_id, Some(91));
    assert_eq!(view.requested_state, Some(4));
    assert_eq!(view.group, 14);
    let observation = view
        .parts
        .try_register_part_contact_spy(&mut backend, 0)
        .unwrap();
    assert_eq!(
        pool.publication(&body).unwrap().rigid_bodies()[0],
        observation
    );
    assert_eq!(
        backend
            .collision_body_pool()
            .try_raw_state(observation)
            .unwrap()
            & 15,
        // Requested state 4 with the fixture's static flag set stores state 1
        // (Part::AddToSimulation 0x82ae0ab8), then RegisterSpyTarget sets bit 3.
        9
    );
    view.parts
        .try_remove_part_contact_spy(&mut backend, 0)
        .unwrap();
    assert_eq!(
        backend
            .collision_body_pool()
            .try_raw_state(observation)
            .unwrap()
            & 15,
        // RemoveSpyTarget clears bit 3, leaving the stored state 1.
        1
    );
    assert_eq!(pool.candidate(&body).unwrap().world_index(), assembly_id);
    let detached = pool.try_remove_serialized(&mut backend, &body).unwrap();
    assert!(!detached.is_attached());
    assert_eq!(detached.entity_id, Some(701));
    assert_eq!(detached.body_id, Some(91));
}

#[test]
fn constructed_and_source_members_share_ids_refresh_and_swap_last_storage() {
    let (mut backend, mut manager, source) = super::super::tests::fixture(6, 1);
    let mut pool = SimulationDmoAssemblyPool::try_new_vehicle(&manager, &backend, false).unwrap();
    assert_eq!(pool.capacity(), 24);
    let source_handle = pool
        .try_insert(
            &mut manager,
            &mut backend,
            SimulationDmoAssemblyCandidate::try_from_locator(&source).unwrap(),
            4,
            &mut HostSerialisedAssemblyBBoxNumeric,
        )
        .unwrap();
    let def = definition();
    let first = create(&def, &mut backend, 2.).unwrap();
    let first = pool
        .try_insert_character(
            &mut manager,
            &mut backend,
            CharacterAssemblyCandidate::new(first, 1),
            4,
        )
        .unwrap();
    let second = create(&def, &mut backend, 2.).unwrap();
    let second = pool
        .try_insert_character(
            &mut manager,
            &mut backend,
            CharacterAssemblyCandidate::new(second, 2),
            2,
        )
        .unwrap();
    let source_id = pool.candidate(&source_handle).unwrap().world_index();
    assert_eq!(
        pool.character_candidate(&first).unwrap().world_index(),
        source_id.wrapping_add(1)
    );
    assert_eq!(
        pool.character_candidate(&second).unwrap().world_index(),
        source_id.wrapping_add(2)
    );
    assert_eq!(
        pool.character_candidate(&first).unwrap().requested_state(),
        Some(4)
    );
    // Grab Init must read the SAME constructed Part, not the source-only
    // candidate accessor or a synthesized DMO. Both memberships share reader.
    let grab = pool
        .try_grab_part_snapshot(&backend, &first)
        .unwrap()
        .unwrap();
    assert_eq!(
        grab.body,
        Some(pool.character_publication(&first).unwrap().rigid_bodies()[0])
    );
    assert_eq!(grab.inertia.inverse_mass, 0.5);
    assert_eq!(
        grab.inverse_tensor_w.to_bits(),
        def.lock().unwrap().parts[0].mass.inverse_tensor_w.to_bits()
    );
    assert!(
        pool.try_grab_part_snapshot(&backend, &source_handle)
            .unwrap()
            .unwrap()
            .body
            .is_some()
    );
    backend.begin_dynamic_update_frame(1. / 60.);
    assert_eq!(
        pool.try_update_serialized(&backend, &mut HostSerialisedAssemblyBBoxNumeric)
            .unwrap(),
        vec![0, 1]
    );
    let frame = backend.collision_body_pool().frame_identity().unwrap();
    assert!(
        pool.character_publication(&first)
            .unwrap()
            .has_bounds_for_frame(frame)
    );
    assert!(
        pool.character_publication(&second)
            .unwrap()
            .all_parts_frozen()
    );
    assert_eq!(pool.character_publication(&first).unwrap().group(), 10);
    assert_eq!(
        pool.character_publication(&first).unwrap().records()[0].group,
        10
    );
    let bodies = pool
        .character_publication(&second)
        .unwrap()
        .rigid_bodies()
        .to_vec();
    assert!(matches!(
        pool.try_remove_serialized(&mut backend, &first),
        Err(SimulationDmoPoolError::WrongMemberKind)
    ));
    let removed = pool.try_remove_character(&mut backend, &first).unwrap();
    assert!(!removed.assembly.parts.is_attached());
    assert!(matches!(
        pool.try_grab_part_snapshot(&backend, &first),
        Err(DynamicAssemblySourcePartError::NotAttached)
    ));
    let detached_grab = removed
        .assembly
        .parts
        .try_grab_part_snapshot(&backend, 0)
        .unwrap();
    assert!(detached_grab.body.is_none());
    assert!(detached_grab.body_transform.is_none());
    assert_eq!(detached_grab.inertia.inverse_mass, 0.5);
    assert_eq!(pool.len(), 2);
    assert!(pool.handles().nth(1).unwrap().same_as(&second));
    assert_eq!(
        pool.character_publication(&second).unwrap().rigid_bodies(),
        bodies
    );
    assert_eq!(pool.publications().count(), 1);
    assert_eq!(pool.character_publications().count(), 1);
    pool.try_remove_character(&mut backend, &second).unwrap();
    pool.try_remove_serialized(&mut backend, &source_handle)
        .unwrap();
}

#[test]
fn foreign_character_binding_refuses_before_any_id_or_body_write() {
    let (mut backend, mut manager, _) = super::super::tests::fixture(4, 1);
    let (mut foreign, _, _) = super::super::tests::fixture(4, 1);
    let mut pool = SimulationDmoAssemblyPool::try_new_vehicle(&manager, &backend, false).unwrap();
    let candidate = create(&definition(), &mut foreign, 2.).unwrap();
    let before = backend.rigid_body_snapshot().counts;
    let failed = pool
        .try_insert_character(
            &mut manager,
            &mut backend,
            CharacterAssemblyCandidate::new(candidate, 1),
            4,
        )
        .unwrap_err();
    assert!(matches!(
        failed.error,
        CharacterPoolError::Pool(SimulationDmoPoolError::ForeignBackend)
    ));
    assert_eq!(failed.candidate.world_index(), 0xffff);
    assert!(!failed.candidate.assembly.parts.is_attached());
    assert_eq!(backend.rigid_body_snapshot().counts, before);
    assert_eq!(pool.len(), 0);
}
