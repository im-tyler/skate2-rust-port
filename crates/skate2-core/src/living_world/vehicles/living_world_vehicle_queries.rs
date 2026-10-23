//! Shared Manager vehicle queries over the existing live EntityMan.
use super::{LivingWorldGrindServices, VehicleNearQueryError};
use crate::{
    debug_overlay::{DebugOverlayError, DebugOverlaySlot},
    input_cvar_runtime::{InputCvarError, InputCvarReferences},
};

#[derive(Debug)]
pub(crate) enum VehicleSphereQueryError {
    Range(VehicleNearQueryError),
    Cvar(InputCvarError),
    Overlay(DebugOverlayError),
}

impl LivingWorldGrindServices {
    /// LWVehicleMan::AddEntityVolumesToBatch82d22c08. Uses the retained
    /// native-order EntityMan, regardless of presentation visibility.
    pub(crate) fn add_vehicle_avoidance_volumes(
        &self, batch: &mut crate::collision_volume_line::VolumeLineBatch,
    ) -> Result<(), VehicleNearQueryError> {
        let vehicles = self.grind_vehicle_entities.as_ref()
            .ok_or(VehicleNearQueryError::MissingEntityMan)?;
        for id in vehicles.ids() {
            let Some(vehicle) = vehicles.get(id) else { continue; };
            let mut rows = vehicle.core.transform;
            for row in &mut rows[..3] { row[3] = 0.; }
            rows[3][3] = 1.;
            let half = vehicle.dimensions().map(|v| v * 0.5);
            // Source computes rotated local(0, halfHeight, 0) in Y,X,Z
            // order, then adds translation separately (not a single FMA).
            rows[3] = std::array::from_fn(|i| {
                let y = half[1] * rows[1][i];
                let x = 0.0_f32.mul_add(rows[0][i], y);
                let z = 0.0_f32.mul_add(rows[2][i], x);
                rows[3][i] + z
            });
            // Unlike peds/DMOs, original vehicles use the batch null mask.
            if !batch.add_volume(crate::collision_volume_line::VolumeData {
                transform: std::array::from_fn(|i| rows[i / 4][i % 4]),
                dimensions: half, mask: batch.null_mask,
            }) { break; }
        }
        Ok(())
    }

    /// Manager::DoesAnyVehicleIntersectSphere82710760. Shared by breadcrumb
    /// respawn and UpdateCollisionStatus; the caller supplies the sphere centre
    /// (only UpdateCollisionStatus lifts its root by the radius).
    pub(crate) fn try_vehicle_intersects_sphere(
        &self,
        position: [f32; 3],
        radius: f32,
        cvars: &InputCvarReferences,
        overlay: &DebugOverlaySlot,
    ) -> Result<bool, VehicleSphereQueryError> {
        use VehicleSphereQueryError as E;
        let mut count = 0;
        let mut ids = [0; 6];
        let mut distances = [0.; 6];
        self.try_get_vehicles_within_range(
            [position[0], position[2]],
            radius + 8.,
            6,
            &mut count,
            &mut ids,
            &mut distances,
        )
        .map_err(E::Range)?;
        let vehicles = self
            .grind_vehicle_entities
            .as_ref()
            .ok_or(E::Range(VehicleNearQueryError::MissingEntityMan))?;
        for id in &ids[..count as usize] {
            // GetVehicle82710860: a removed KD entry is skipped. No additional
            // active/visible filter is present in the native sphere query.
            let Some(vehicle) = vehicles.get(*id) else {
                continue;
            };
            let matrix = &vehicle.core.transform;
            let dimensions = vehicle.dimensions();
            if cvars.try_draw_lw_position_test().map_err(E::Cvar)? {
                overlay.require().map_err(E::Overlay)?;
            }
            if sphere_overlaps_vehicle(matrix, dimensions, position, radius) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn sphere_overlaps_vehicle(
    matrix: &[[f32; 4]; 4],
    dimensions: [f32; 4],
    position: [f32; 3],
    radius: f32,
) -> bool {
    let local = vehicle_local_point(matrix, position, matrix[3]);
    // 82710990..10a2c: ordered strict Z, X, Y comparisons. This is the
    // source's expanded local box, not a Euclidean sphere/OBB distance test.
    // 827107a8 loads 0x3f800000: the extra vertical clearance is exactly 1m.
    dimensions[2].mul_add(0.5, radius) > local[2].abs()
        && dimensions[0].mul_add(0.5, radius) > local[0].abs()
        && 1. + dimensions[1].mul_add(0.5, radius) > local[1].abs()
}

/// Native transpose/translate: first multiply, then Y and Z fused adds.
/// Also used by the existing IsPositionInsideAnyVehicle query.
pub(crate) fn vehicle_local_point(
    matrix: &[[f32; 4]; 4],
    point: [f32; 3],
    centre: [f32; 4],
) -> [f32; 3] {
    let delta: [f32; 3] = std::array::from_fn(|i| point[i] - centre[i]);
    std::array::from_fn(|axis| {
        matrix[axis][2].mul_add(
            delta[2],
            matrix[axis][1].mul_add(delta[1], matrix[axis][0] * delta[0]),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::living_world_grind_services::{
        entities::EntityMan, vehicle_entity::factory::tests::vehicle_fixture,
    };

    #[test]
    fn populated_range_uses_existing_tree_and_preserves_unused_output_entries() {
        let mut service = LivingWorldGrindServices::new();
        let mut entities = EntityMan::try_construct().unwrap();
        entities.try_initialize().unwrap();
        for (id, x) in [(31, 5.), (32, 2.), (33, 30.)] {
            let mut vehicle = vehicle_fixture();
            vehicle.id = id;
            // Only the native SetTransform writes mPositionXZ (the KD key);
            // assigning the field directly leaves every vehicle at [0, 0].
            let mut transform = CAR;
            transform[3] = [x, 0., 0., 1.];
            vehicle.core.set_transform::<()>(transform).unwrap();
            entities.add(id, vehicle).unwrap();
        }
        service.grind_vehicle_entities = Some(entities);
        let mut count = 99;
        let mut ids = [999; 6];
        let mut distances = [-1.; 6];
        service
            .try_get_vehicles_within_range([0., 0.], 10., 6, &mut count, &mut ids, &mut distances)
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(ids, [32, 31, 999, 999, 999, 999]);
        assert!(distances[0] < distances[1]);
        assert_eq!(&distances[2..], &[-1.; 4]);

        // Removing a map member does not rebuild the source KD tree. The range
        // still returns that ID; the sphere query must skip its failed Get.
        service.grind_vehicle_entities.as_mut().unwrap().remove(32);
        service
            .try_get_vehicles_within_range([0., 0.], 10., 6, &mut count, &mut ids, &mut distances)
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(&ids[..2], &[32, 31]);
        service
            .try_get_vehicles_within_range(
                [100., 100.],
                1.,
                257,
                &mut count,
                &mut ids,
                &mut distances,
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(&ids[..2], &[32, 31]);
    }

    // A 90-degree car, centred away from the world origin. Its long local Z
    // axis is world X; dimensions are width2, height2, length6.
    const CAR: [[f32; 4]; 4] = [
        [0., 0., -1., 0.],
        [0., 1., 0., 0.],
        [1., 0., 0., 0.],
        [10., 2., 20., 1.],
    ];
    const SIZE: [f32; 4] = [2., 2., 6., 0.];

    #[test]
    fn nearby_vehicle_only_blocks_candidates_inside_its_rotated_expanded_box() {
        assert!(sphere_overlaps_vehicle(&CAR, SIZE, [13., 2., 20.], 0.5));
        // Still within the radius+8 broadphase, outside the car's width.
        assert!(!sphere_overlaps_vehicle(&CAR, SIZE, [10., 2., 23.], 0.5));
        // Exact expanded length boundary is not an overlap.
        assert!(!sphere_overlaps_vehicle(&CAR, SIZE, [13.5, 2., 20.], 0.5));
    }

    #[test]
    fn vertical_clearance_and_unordered_coordinates_follow_source_comparisons() {
        assert!(sphere_overlaps_vehicle(&CAR, SIZE, [10., 4., 20.], 0.5));
        assert!(!sphere_overlaps_vehicle(&CAR, SIZE, [10., 4.5, 20.], 0.5));
        assert!(!sphere_overlaps_vehicle(
            &CAR,
            SIZE,
            [10., f32::NAN, 20.],
            0.5
        ));
    }
}
