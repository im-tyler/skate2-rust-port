//! Vehicle.EvaluateSkitchingIntents82d23cc0. Actual PhysOut state/intents and
//! Actor getters are read lazily, in manager order; no synthetic skater proxy.
use crate::physics_state_ids::PHYSICS_STATE_SKITCHING;
use super::Vehicle;
use crate::{
    attribute_runtime::AttributeRuntimeError,
    living_world_grind_services::vehicle_motion::{
        VehicleCharacteristics, vision::geometry::point_in_quad,
    },
    reckoning_numeric::{ReckoningNumeric, length_from_squared_and_estimate, try_length},
};

/// Borrowed access to the existing SkaterManager/Actor/PhysOut owners. The
/// eventual frame adapter must use actor order and the completed PhysOut.
pub(crate) trait SkitchActors {
    type Error;
    type Actor: Copy;
    fn count(&mut self) -> Result<u32, Self::Error>;
    fn actor(&mut self, index: u32) -> Result<Self::Actor, Self::Error>;
    fn physics_state(&mut self, actor: Self::Actor) -> Result<u32, Self::Error>;
    fn position(&mut self, actor: Self::Actor) -> Result<[f32; 4], Self::Error>;
    fn effective_deck_z(&mut self, actor: Self::Actor) -> Result<[f32; 4], Self::Error>;
    fn velocity(&mut self, actor: Self::Actor) -> Result<[f32; 4], Self::Error>;
    fn has_world_grab_intent(&mut self, actor: Self::Actor) -> Result<bool, Self::Error>;
}

/// A borrowed view in the existing SkaterManager's order. This neither owns
/// Actors nor constructs a second roster or PhysOut. Caller supplies the
/// registered actors; each getter reads the same retained physical owner.
pub(crate) struct BorrowedSkitchActors<'a> {
    pub(crate) actors: &'a [&'a crate::actor_construction::ActorConstruction],
}
#[derive(Debug)]
pub(crate) enum SkitchActorReadError {
    Index(u32),
    CountOverflow,
    Physical(crate::actor_construction::physical_mount::ActorPhysicalMountError),
    PoisonedOutput,
    Output(
        crate::physical_player_phys_out::construct::ConcretePhysOutError<std::convert::Infallible>,
    ),
}
impl BorrowedSkitchActors<'_> {
    fn read<T>(
        &self,
        index: u32,
        read: impl FnOnce(
            &crate::physical_player_phys_out::PhysicalPlayerPhysOut,
        ) -> Result<
            T,
            crate::physical_player_phys_out::construct::ConcretePhysOutError<
                std::convert::Infallible,
            >,
        >,
    ) -> Result<T, SkitchActorReadError> {
        use SkitchActorReadError as E;
        let actor = self.actors.get(index as usize).ok_or(E::Index(index))?;
        let physical = actor.cached_physical_allocation().map_err(E::Physical)?;
        let output = physical
            .owner()
            .phys_out()
            .lock()
            .map_err(|_| E::PoisonedOutput)?;
        read(&output).map_err(E::Output)
    }
}
impl SkitchActors for BorrowedSkitchActors<'_> {
    type Error = SkitchActorReadError;
    type Actor = u32;
    fn count(&mut self) -> Result<u32, Self::Error> {
        u32::try_from(self.actors.len()).map_err(|_| SkitchActorReadError::CountOverflow)
    }
    fn actor(&mut self, index: u32) -> Result<u32, Self::Error> {
        self.actors
            .get(index as usize)
            .ok_or(SkitchActorReadError::Index(index))
            .map(|_| index)
    }
    fn physics_state(&mut self, actor: u32) -> Result<u32, Self::Error> {
        self.read(
            actor,
            |out| Ok(out.try_state_output()?.physics_state as u32),
        )
    }
    fn position(&mut self, actor: u32) -> Result<[f32; 4], Self::Error> {
        use crate::reckoning_numeric::ReckoningVector;
        // Actor.GetPosition825f9df0 = Skeleton+1a0 (root translation),
        // NOT GetCentreOfMass used by the simulation-center census helper.
        self.read(actor, |out| {
            Ok(out.try_skeleton_output()?.skeleton_root_matrix.columns()[3].operand())
        })
    }
    fn effective_deck_z(&mut self, actor: u32) -> Result<[f32; 4], Self::Error> {
        use crate::reckoning_numeric::ReckoningVector;
        // Actor.GetEffectiveDeckTransform825ed448 = SkateboardReckoning+40.
        self.read(actor, |out| {
            Ok(out.try_camera_board_output()?.effective_transform.columns()[2].operand())
        })
    }
    fn velocity(&mut self, actor: u32) -> Result<[f32; 4], Self::Error> {
        use crate::reckoning_numeric::ReckoningVector;
        // Actor.GetVelocity825ed338 = SystemReckoning+0, with NO census
        // threshold that would zero a small but nonzero velocity.
        self.read(actor, |out| {
            Ok(out
                .try_camera_system_output()?
                .damped_centre_of_mass_velocity
                .operand())
        })
    }
    fn has_world_grab_intent(&mut self, actor: u32) -> Result<bool, Self::Error> {
        self.read(actor, |out| {
            Ok(out.try_camera_intents_output()?.has_world_grab_intent)
        })
    }
}

#[derive(Debug)]
pub(crate) enum SkitchPerceptionError<A, N> {
    Actor(A),
    Numeric(N),
    Attribute(AttributeRuntimeError),
    MissingSkitchVision,
}

impl Vehicle {
    pub(crate) fn evaluate_skitching_intents<A: SkitchActors, N: ReckoningNumeric>(
        &mut self,
        actors: &mut A,
        numeric: &mut N,
    ) -> Result<(), SkitchPerceptionError<A::Error, N::Error>> {
        use SkitchPerceptionError as E;
        self.motion.rear_skater_speed = 0.;
        self.motion.rear_skater_distance = -1.;
        self.motion.rear_skater_skitch_potential = false;
        self.motion.rear_skater_tends_to_skitch = false;
        if self.motion.being_skitched {
            return Ok(());
        }
        let count = actors.count().map_err(E::Actor)?;
        for index in 0..count {
            let actor = actors.actor(index).map_err(E::Actor)?;
            // PDB PHYSICS_STATE_SKITCHING, not physics CATEGORY.
            if actors.physics_state(actor).map_err(E::Actor)? == PHYSICS_STATE_SKITCHING.cast_unsigned() {
                continue;
            }
            let vision_position = actors.position(actor).map_err(E::Actor)?;
            let half_length = self.length() * 0.5;
            let target_position = self.core.target_transform[3];
            // Native calls GetPosition a second time for the distance test.
            let position = actors.position(actor).map_err(E::Actor)?;
            let delta: [f32; 4] = std::array::from_fn(|i| position[i] - target_position[i]);
            let gap = try_length(delta, numeric).map_err(E::Numeric)? - half_length;
            let radius = self
                .characteristics
                .scalar(0x3346_6832_d817_8eaf)
                .map_err(E::Attribute)?;
            if !(radius > gap) {
                continue;
            }
            let quad = self
                .vision
                .skitch_helper
                .as_ref()
                .ok_or(E::MissingSkitchVision)?;
            if !point_in_quad(vision_position, quad, numeric).map_err(E::Numeric)? {
                continue;
            }
            let deck_z = actors.effective_deck_z(actor).map_err(E::Actor)?;
            let forward = self.core.target_transform[2];
            let velocity = actors.velocity(actor).map_err(E::Actor)?;
            let squared_speed = numeric.dot3(velocity, velocity).map_err(E::Numeric)?;
            let alignment = numeric.dot3(deck_z, forward).map_err(E::Numeric)?;
            self.motion.rear_skater_distance = gap;
            let seed = numeric
                .reciprocal_sqrt_estimate(squared_speed)
                .map_err(E::Numeric)?;
            self.motion.rear_skater_speed = length_from_squared_and_estimate(squared_speed, seed);
            if alignment > 0. {
                // A later unaligned candidate still replaces speed/distance
                // but does not clear a prior aligned candidate's two flags.
                self.motion.rear_skater_skitch_potential = true;
                self.motion.rear_skater_tends_to_skitch =
                    actors.has_world_grab_intent(actor).map_err(E::Actor)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::factory::tests::vehicle_fixture;
    use super::*;
    use crate::{
        attribute_runtime::tests::{collection, graph, node},
        reckoning_numeric::HostReckoningNumeric,
    };
    use std::{convert::Infallible, sync::Arc};
    use crate::physics_state_ids::PHYSICS_STATE_PHYSICS_GROUND;

    struct Actors {
        reads: Vec<(u32, &'static str)>,
    }
    impl SkitchActors for Actors {
        type Error = Infallible;
        type Actor = u32;
        fn count(&mut self) -> Result<u32, Infallible> {
            self.reads.push((0, "count"));
            Ok(3)
        }
        fn actor(&mut self, i: u32) -> Result<u32, Infallible> {
            self.reads.push((i, "actor"));
            Ok(i)
        }
        fn physics_state(&mut self, i: u32) -> Result<u32, Infallible> {
            self.reads.push((i, "state"));
            Ok(if i == 2 { PHYSICS_STATE_SKITCHING.cast_unsigned() } else { PHYSICS_STATE_PHYSICS_GROUND.cast_unsigned() })
        }
        fn position(&mut self, i: u32) -> Result<[f32; 4], Infallible> {
            self.reads.push((i, "position"));
            Ok([0., 0., 2. + i as f32, 0.])
        }
        fn effective_deck_z(&mut self, i: u32) -> Result<[f32; 4], Infallible> {
            self.reads.push((i, "deck"));
            Ok([0., 0., if i == 0 { 1. } else { -1. }, 0.])
        }
        fn velocity(&mut self, i: u32) -> Result<[f32; 4], Infallible> {
            self.reads.push((i, "velocity"));
            Ok([0., 0., 4. + i as f32, 0.])
        }
        fn has_world_grab_intent(&mut self, i: u32) -> Result<bool, Infallible> {
            self.reads.push((i, "intent"));
            Ok(true)
        }
    }

    #[test]
    fn later_unaligned_skater_replaces_speed_but_keeps_prior_skitch_flags() {
        let mut vehicle = vehicle_fixture();
        // The skitch radius key needs its class definition present, as real
        // Vault data always pairs node and definition.
        let mut vault = graph(
            1,
            vec![crate::attribute_runtime::tests::definition(
                0x3346_6832_d817_8eaf,
                4,
                4,
            )],
        );
        collection(
            &mut vault,
            1,
            2,
            0,
            None,
            vec![node(0x3346_6832_d817_8eaf, 0x40, 10_f32.to_bits())],
        );
        vehicle.characteristics = Arc::new(vault).instance(1, 2);
        vehicle.vision.skitch_helper = Some([
            [-1., 0., 0., 0.],
            [1., 0., 0., 0.],
            [-1., 0., 8., 0.],
            [1., 0., 8., 0.],
        ]);
        let mut actors = Actors { reads: Vec::new() };
        vehicle
            .evaluate_skitching_intents(&mut actors, &mut HostReckoningNumeric)
            .unwrap();
        assert_eq!(vehicle.motion.rear_skater_distance, 3.);
        assert_eq!(vehicle.motion.rear_skater_speed, 5.);
        assert!(vehicle.motion.rear_skater_skitch_potential);
        assert!(vehicle.motion.rear_skater_tends_to_skitch);
        assert_eq!(
            actors
                .reads
                .iter()
                .filter(|&&(i, s)| i == 0 && s == "position")
                .count(),
            2
        );
        assert!(!actors.reads.contains(&(1, "intent")));
        assert!(!actors.reads.contains(&(2, "position")));
        actors.reads.clear();
        vehicle.motion.being_skitched = true;
        vehicle
            .evaluate_skitching_intents(&mut actors, &mut HostReckoningNumeric)
            .unwrap();
        assert!(actors.reads.is_empty());
        assert_eq!(vehicle.motion.rear_skater_distance, -1.);
        assert!(!vehicle.motion.rear_skater_skitch_potential);
    }
}
