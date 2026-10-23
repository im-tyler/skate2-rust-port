#![expect(clippy::float_cmp, reason = "exact native clamp and branch values")]
use super::*;

#[derive(Debug, PartialEq)]
enum Event {
    Resolve(Callback),
    Motion(String, f32),
    Settable(String, u32, bool),
    SetAttributes,
    Overlay,
}

struct Source {
    grinding: bool,
    flipped: bool,
    mirrored: bool,
    velocity: [f32; 3],
    nose: [f32; 3],
    crouch_intent: Option<f32>,
    twist_intent: Option<f32>,
    grind_crouch: f32,
    centre_of_mass_to_deck: f32,
    shoulder_twist: f32,
    use_phys: bool,
    show: bool,
    dt: f32,
    /// Twist coordinate of the base tree for the last normalized twist write.
    range: [f32; 2],
    twist_found: bool,
    normalized_twist: Option<f32>,
    events: Vec<Event>,
}
impl Default for Source {
    fn default() -> Self {
        Self {
            grinding: true,
            flipped: false,
            mirrored: false,
            velocity: [0.0; 3],
            nose: [0.0; 3],
            crouch_intent: None,
            twist_intent: None,
            grind_crouch: 0.0,
            centre_of_mass_to_deck: 0.0,
            shoulder_twist: 0.0,
            use_phys: true,
            show: false,
            dt: 1.0 / 60.0,
            range: [-0.5, 0.5],
            twist_found: true,
            normalized_twist: None,
            events: Vec::new(),
        }
    }
}
impl GrindingSource for Source {
    type Error = &'static str;
    type Base = ();
    fn resolve(&mut self, callback: Callback) -> Result<(), Self::Error> {
        self.events.push(Event::Resolve(callback));
        Ok(())
    }
    fn is_grinding(&mut self) -> Result<bool, Self::Error> {
        Ok(self.grinding)
    }
    fn skating_grind_name(&mut self) -> Result<FastString30, Self::Error> {
        Ok(FastString30::encode("Feeble").unwrap())
    }
    fn grind_crouch(&mut self) -> Result<f32, Self::Error> {
        Ok(self.grind_crouch)
    }
    fn skateboard_flipped(&mut self) -> Result<bool, Self::Error> {
        Ok(self.flipped)
    }
    fn deck_velocity(&mut self) -> Result<[f32; 3], Self::Error> {
        Ok(self.velocity)
    }
    fn deck_nose_direction(&mut self) -> Result<[f32; 3], Self::Error> {
        Ok(self.nose)
    }
    fn centre_of_mass_to_deck(&mut self) -> Result<f32, Self::Error> {
        Ok(self.centre_of_mass_to_deck)
    }
    fn shoulder_twist(&mut self) -> Result<f32, Self::Error> {
        Ok(self.shoulder_twist)
    }
    fn mirrored(&mut self) -> Result<bool, Self::Error> {
        Ok(self.mirrored)
    }
    fn motion_attribute(&mut self, name: FastString30, value: f32) -> Result<(), Self::Error> {
        self.events
            .push(Event::Motion(name.decode().unwrap(), value));
        Ok(())
    }
    fn intent(&mut self, name: FastString36) -> Result<Option<f32>, Self::Error> {
        // PackedFastString discards case at encode and decodes the canonical
        // lowercase form, so the stub matches lowercase like the packed
        // comparison the native performs.
        Ok(match name.decode().unwrap().as_str() {
            "crouch" => self.crouch_intent,
            "grindtwist" => self.twist_intent,
            _ => return Err("intent"),
        })
    }
    fn scalar(&mut self, collection: Collection, key: u64) -> Result<f32, Self::Error> {
        // Values of the shipped Vault collections.
        Ok(match (collection, key) {
            (Collection::GrindCrouch, CROUCH_DRIVE_SPEED | CROUCH_MINIMUM) => 0.6,
            (Collection::GrindCrouch, CROUCH_MAXIMUM) => 0.95,
            (Collection::ControlFade, FADE_INTENT_RATE) => 0.3,
            (Collection::ControlFade, FADE_BLEND) => 0.15,
            (Collection::ControlFade, FADE_DELTA_CHANGE) => 0.05,
            (Collection::ControlFade, FADE_DELTA_LIMIT) => 0.5,
            _ => return Err("scalar"),
        })
    }
    fn cvar(&mut self, cvar: GrindingCvar) -> Result<bool, Self::Error> {
        Ok(match cvar {
            GrindingCvar::UsePhysInducedGrindCrouch => self.use_phys,
            GrindingCvar::ShowGrindCrouchText | GrindingCvar::ShowGrindControlFade => self.show,
        })
    }
    fn dt(&mut self) -> Result<f32, Self::Error> {
        Ok(self.dt)
    }
    fn settable(
        &mut self,
        name: FastString30,
        value: f32,
        normalized: bool,
    ) -> Result<(), Self::Error> {
        let name = name.decode().unwrap();
        if name == "twist" && normalized {
            self.normalized_twist = Some(value);
        }
        self.events
            .push(Event::Settable(name, value.to_bits(), normalized));
        Ok(())
    }
    fn set_attributes(&mut self) -> Result<(), Self::Error> {
        self.events.push(Event::SetAttributes);
        Ok(())
    }
    fn base_animation(&mut self) -> Result<Self::Base, Self::Error> {
        Ok(())
    }
    fn base_attribute(
        &mut self,
        _base: &Self::Base,
        name: FastString30,
    ) -> Result<Option<f32>, Self::Error> {
        assert_eq!(name.decode().unwrap(), "twist");
        if !self.twist_found {
            return Ok(None);
        }
        Ok(match self.normalized_twist {
            Some(value) if value == 0.0 => Some(self.range[0]),
            Some(value) if value == 1.0 => Some(self.range[1]),
            _ => None,
        })
    }
    fn debug_overlay(&mut self) -> Result<(), Self::Error> {
        self.events.push(Event::Overlay);
        Ok(())
    }
    fn assertion(&mut self, site: &'static str) -> Self::Error {
        site
    }
}

fn names() -> ControlFadeNames {
    ControlFadeNames {
        distance_board_to_cog: FastString30::encode("disttocog").unwrap(),
        twist: FastString30::encode("twist").unwrap(),
        twist_intent: FastString36::encode("GrindTwist").unwrap(),
    }
}

#[test]
fn grind_attributes_are_skipped_off_a_grind() {
    let mut s = Source {
        grinding: false,
        ..Source::default()
    };
    create_grind_attributes_update(&mut s).unwrap();
    assert_eq!(
        s.events,
        [Event::Resolve(Callback::CreateGrindAttributesUpdate)]
    );
}

#[test]
fn grind_facing_follows_the_cross_product_and_the_stance_xor() {
    // cross(velocity, nose).y = vz*nx - vx*nz = 1 for a +z velocity and +x nose.
    let mut s = Source {
        velocity: [0.0, 0.0, 1.0],
        nose: [1.0, 0.0, 0.0],
        ..Source::default()
    };
    create_grind_attributes_update(&mut s).unwrap();
    assert_eq!(s.events[1], Event::Motion("feeble".into(), 1.0));
    assert_eq!(
        s.events[2],
        Event::Motion("grindfacingbackwards".into(), 1.0)
    );
    let mut s = Source {
        velocity: [0.0, 0.0, 1.0],
        nose: [1.0, 0.0, 0.0],
        mirrored: true,
        ..Source::default()
    };
    create_grind_attributes_update(&mut s).unwrap();
    assert_eq!(
        s.events[2],
        Event::Motion("grindfacingforwards".into(), 1.0)
    );
    // Mirrored and flipped cancel; an unordered y takes the ble branch.
    let mut s = Source {
        velocity: [f32::NAN, 0.0, 0.0],
        nose: [0.0, 0.0, 1.0],
        mirrored: true,
        flipped: true,
        ..Source::default()
    };
    create_grind_attributes_update(&mut s).unwrap();
    assert_eq!(
        s.events[2],
        Event::Motion("grindfacingforwards".into(), 1.0)
    );
}

#[test]
fn grind_crouch_drives_toward_the_lower_of_input_and_physics() {
    let mut s = Source {
        centre_of_mass_to_deck: 0.9,
        ..Source::default()
    };
    let mut v = GrindCrouchInstance::default();
    grind_crouch_begin(&mut s, Some(&mut v)).unwrap();
    assert_eq!(v.distance, 0.9);
    let drive = FastString30::encode("disttocog").unwrap();
    s.crouch_intent = Some(1.0);
    s.grind_crouch = 0.2;
    s.events.clear();
    grind_crouch_update(&mut s, Some(&mut v), drive).unwrap();
    // Target min(clamp(0, .6, .95), clamp(.8, .6, .95)) = .6, one step of dt*.6.
    let step = (1.0f32 / 60.0) * 0.6;
    assert_eq!(v.distance, 0.9 - step);
    assert_eq!(
        s.events,
        [
            Event::Resolve(Callback::GrindCrouchUpdate),
            Event::Settable("disttocog".into(), (0.9 - step).to_bits(), false),
        ]
    );
    s.use_phys = false;
    s.crouch_intent = None;
    s.show = true;
    s.events.clear();
    let before = v.distance;
    grind_crouch_update(&mut s, Some(&mut v), drive).unwrap();
    // Without the physics term the input alone (1 - 0 clamped to .95) leads.
    assert_eq!(v.distance, before + step);
    assert_eq!(s.events.last(), Some(&Event::Overlay));
}

#[test]
fn control_fade_samples_the_twist_range_then_blends_the_intent() {
    let mut s = Source {
        centre_of_mass_to_deck: 0.75,
        shoulder_twist: 0.25,
        mirrored: true,
        ..Source::default()
    };
    let names = names();
    let mut v = GrindControlFadeInstance::after_allocation();
    control_fade_begin(&mut s, Some(&mut v), &names).unwrap();
    assert_eq!(
        s.events,
        [
            Event::Resolve(Callback::ControlFadeBegin),
            Event::Settable("disttocog".into(), 0.75f32.to_bits(), false),
            Event::Settable("twist".into(), 0.0f32.to_bits(), true),
            Event::SetAttributes,
            Event::Settable("twist".into(), 1.0f32.to_bits(), true),
            Event::SetAttributes,
            // Mirrored: PI - 0.25, clamped to the sampled maximum.
            Event::Settable("twist".into(), 0.5f32.to_bits(), false),
        ]
    );
    assert_eq!((v.minimum, v.maximum, v.twist), (-0.5, 0.5, 0.5));
    assert_eq!((v.adjusted, v.delta), (Some(0.5), Some(0.0)));
    // The first Update only clears the flag.
    s.events.clear();
    control_fade_update(&mut s, Some(&mut v), &names).unwrap();
    assert!(s.events.is_empty() && !v.first_update && v.time == 0.0);
    s.twist_intent = Some(-2.0);
    control_fade_update(&mut s, Some(&mut v), &names).unwrap();
    let adjusted = 0.3f32.mul_add(-2.0, 0.5);
    assert_eq!(v.adjusted, Some(adjusted));
    // Blended step -0.09 is limited to 0.05 around the previous zero delta.
    assert_eq!(v.delta, Some(-0.05));
    assert_eq!(v.twist, 0.5 - 0.05);
    assert_eq!(v.time, 1.0 / 60.0);
    assert_eq!(
        s.events.last(),
        Some(&Event::Settable(
            "twist".into(),
            (0.5f32 - 0.05).to_bits(),
            false
        ))
    );
}

#[test]
fn control_fade_requires_the_sampled_twist_attribute() {
    let mut s = Source {
        twist_found: false,
        ..Source::default()
    };
    let mut v = GrindControlFadeInstance::after_allocation();
    assert_eq!(
        control_fade_begin(&mut s, Some(&mut v), &names()),
        Err("GrindControlFade minimum twist attribute")
    );
    assert!(v.first_update);
    assert_eq!(
        control_fade_begin(&mut s, None, &names()),
        Err("GrindControlFade instance")
    );
}
