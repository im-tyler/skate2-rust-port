#![expect(
    clippy::float_cmp,
    reason = "exact source stores and unchanged values are the test oracle"
)]
use super::*;
use crate::reckoning_numeric::HostReckoningNumeric;

struct Fixture {
    filter: Option<VectorPidFilter>,
    target: Option<Vector>,
    rotation: Option<[f32; 4]>,
    velocity: Option<[f32; 4]>,
    valid: Option<bool>,
    output: Vector,
}
impl Fixture {
    fn new() -> Self {
        Self {
            filter: Some(VectorPidFilter::new(
                PhysicsControllerData {
                    proportional: 0.0,
                    integral: 0.0,
                    derivative: 0.0,
                    integral_blend: 0.0,
                },
                [0.0; 4],
            )),
            target: Some(Vector::known([0.0; 4])),
            rotation: Some([0.0; 4]),
            velocity: Some([0.0; 4]),
            valid: Some(false),
            output: Vector::known([99.0; 4]),
        }
    }
    fn run(
        &mut self,
        source: &mut Source,
    ) -> Result<
        (),
        HeadTrackingError<&'static str, crate::reckoning_numeric::ReckoningHostNumericError>,
    > {
        // Explicit unknown W demonstrates this branch never requires fabricated
        // affine W. XYZ is immaterial in the invalid-target arm.
        let matrix = Matrix::from_columns(
            [Vector {
                xyz: [0.0; 3],
                w: None,
            }; 4],
        );
        try_update(
            HeadTrackingState {
                filter: &mut self.filter,
                target_position: &mut self.target,
                rotation: &mut self.rotation,
                rotation_velocity: &mut self.velocity,
                valid_target: &mut self.valid,
            },
            &matrix,
            &matrix,
            &mut self.output,
            source,
            &mut HostReckoningNumeric,
        )
    }
}
#[expect(
    clippy::struct_excessive_bools,
    reason = "a test stub mirrors the source trait's independent switches one field each"
)]
struct Source {
    control: [f32; 4],
    force: bool,
    cap: Result<f32, &'static str>,
    draw: bool,
    fail_angle_capture: bool,
    clamp: bool,
    calls: Vec<&'static str>,
}
impl Source {
    fn new() -> Self {
        Self {
            control: [0.0; 4],
            force: false,
            cap: Ok(0.25),
            draw: false,
            fail_angle_capture: false,
            clamp: true,
            calls: vec![],
        }
    }
}
impl HeadTrackingSource for Source {
    type Error = &'static str;
    type ForceTarget = bool;
    type AngleTarget = f32;
    fn controller_layout(&mut self) -> Result<[f32; 4], Self::Error> {
        self.calls.push("layout");
        Ok(self.control)
    }
    fn capture_force_look_at(&mut self) -> Result<bool, Self::Error> {
        self.calls.push("capture force");
        Ok(self.force)
    }
    fn capture_maximum_target_angle(&mut self) -> Result<f32, Self::Error> {
        self.calls.push("capture max");
        if self.fail_angle_capture {
            return Err("angle target unavailable");
        }
        Ok(60.0)
    }
    fn force_look_at(&mut self, target: &bool) -> Result<bool, Self::Error> {
        self.calls.push("force");
        Ok(*target)
    }
    fn maximum_target_angle(&mut self, target: &f32) -> Result<f32, Self::Error> {
        self.calls.push("max");
        Ok(*target)
    }
    fn forced_target(&mut self) -> Result<[f32; 4], Self::Error> {
        self.calls.push("target");
        Ok([1.0, 2.0, 3.0, 4.0])
    }
    fn acceleration_cap(&mut self) -> Result<f32, Self::Error> {
        self.calls.push("cap");
        self.cap
    }
    fn draw_head_adjust(&mut self) -> Result<bool, Self::Error> {
        self.calls.push("draw");
        Ok(self.draw)
    }
    fn clamp_ideal(&mut self) -> Result<bool, Self::Error> {
        Ok(self.clamp)
    }
    fn largest_up_angle(&mut self) -> Result<f32, Self::Error> {
        Ok(45.0)
    }
    fn rotation_limits(&mut self) -> Result<[[f32; 4]; 2], Self::Error> {
        Ok([[-1.0; 4], [1.0; 4]])
    }
}
#[test]
fn constructor_invalid_still_refreshes_pid_reads_cap_and_publishes() {
    let mut fixture = Fixture::new();
    let mut source = Source::new();
    fixture.run(&mut source).unwrap();
    assert_eq!(
        source.calls,
        [
            "layout",
            "capture force",
            "capture max",
            "force",
            "max",
            "cap",
            "draw"
        ]
    );
    assert_eq!(fixture.output.known_lanes().unwrap(), [0.0; 4]);
    assert_eq!(fixture.valid, Some(false));
}
#[test]
fn forced_target_reaches_pid_and_preserves_target_on_cap_failure() {
    let mut fixture = Fixture::new();
    let mut source = Source::new();
    source.force = true;
    source.cap = Err("live cap");
    source.control = [1.0, 2.0, 3.0, 0.5];
    assert!(matches!(
        fixture.run(&mut source),
        Err(HeadTrackingError::Source("live cap"))
    ));
    assert_eq!(
        source.calls,
        [
            "layout",
            "capture force",
            "capture max",
            "force",
            "max",
            "target",
            "cap"
        ]
    );
    assert_eq!(fixture.target, Some(Vector::known([1.0, 2.0, 3.0, 4.0])));
    assert_eq!(fixture.valid, Some(true));
    assert_eq!(fixture.filter.as_ref().unwrap().control().derivative, 3.0);
    assert_eq!(fixture.output.known_lanes().unwrap(), [99.0; 4]);
}
#[test]
fn cap_failure_keeps_completed_pid_update_but_no_rotation_store() {
    let mut fixture = Fixture::new();
    fixture
        .filter
        .as_mut()
        .unwrap()
        .set_value([2.0, 0.0, 0.0, 4.0]);
    let mut source = Source::new();
    source.control = [0.5, 0.0, 0.0, 1.0];
    source.cap = Err("live cap");
    assert!(matches!(
        fixture.run(&mut source),
        Err(HeadTrackingError::Source("live cap"))
    ));
    assert_eq!(
        fixture
            .filter
            .as_ref()
            .unwrap()
            .value()
            .known_lanes()
            .unwrap(),
        [1.0, 0.0, 0.0, 2.0]
    );
    assert_eq!(fixture.rotation, Some([0.0; 4]));
    assert_eq!(fixture.output.known_lanes().unwrap(), [99.0; 4]);
}
#[test]
fn cap_is_on_acceleration_and_debug_stop_retains_published_state() {
    let mut fixture = Fixture::new();
    fixture
        .filter
        .as_mut()
        .unwrap()
        .set_value([4.0, 0.0, 0.0, 8.0]);
    fixture.rotation = Some([1.0, 0.0, 0.0, 2.0]);
    fixture.velocity = Some([1.0, 0.0, 0.0, 2.0]);
    let mut source = Source::new();
    source.draw = true;
    assert!(matches!(
        fixture.run(&mut source),
        Err(HeadTrackingError::DebugArithmeticPending)
    ));
    let result = fixture.output.known_lanes().unwrap();
    assert!((result[0] - 2.25).abs() < 0.00001);
    assert!((result[3] - 4.5).abs() < 0.00001);
    assert_eq!(fixture.rotation, Some(result));
    assert_eq!(fixture.velocity.unwrap()[0], result[0] - 1.0);
    assert_eq!(
        source.calls,
        [
            "layout",
            "capture force",
            "capture max",
            "force",
            "max",
            "cap",
            "draw"
        ]
    );
}
#[test]
fn late_angle_capture_failure_keeps_control_store_without_reading_force() {
    let mut fixture = Fixture::new();
    let mut source = Source::new();
    source.control = [1.0, 2.0, 3.0, 0.5];
    source.force = true;
    source.fail_angle_capture = true;
    assert!(matches!(
        fixture.run(&mut source),
        Err(HeadTrackingError::Source("angle target unavailable"))
    ));
    assert_eq!(source.calls, ["layout", "capture force", "capture max"]);
    assert_eq!(fixture.filter.as_ref().unwrap().control().derivative, 3.0);
    assert_eq!(
        fixture
            .filter
            .as_ref()
            .unwrap()
            .value()
            .known_lanes()
            .unwrap(),
        [0.0; 4]
    );
    assert_eq!(fixture.target, Some(Vector::known([0.0; 4])));
    assert_eq!(fixture.valid, Some(false));
    assert_eq!(fixture.rotation, Some([0.0; 4]));
    assert_eq!(fixture.velocity, Some([0.0; 4]));
    assert_eq!(fixture.output.known_lanes().unwrap(), [99.0; 4]);
}
#[test]
fn negative_cap_is_preserved_and_nan_cap_uses_length() {
    for (cap, expected) in [(-0.5, -0.5), (f32::NAN, 2.0)] {
        let mut fixture = Fixture::new();
        fixture
            .filter
            .as_mut()
            .unwrap()
            .set_value([2.0, 0.0, 0.0, 0.0]);
        let mut source = Source::new();
        source.cap = Ok(cap);
        fixture.run(&mut source).unwrap();
        assert!((fixture.output.xyz[0] - expected).abs() < 0.00001);
    }
}

#[test]
fn valid_target_rotates_head_toward_point_and_clears_latch() {
    let mut fixture = Fixture::new();
    fixture.valid = Some(true);
    fixture.target = Some(Vector {
        xyz: [3.5, 4.866_025_4, 5.0],
        w: None,
    });
    let identity = Matrix::known([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ]);
    let head = Matrix::from_columns([
        identity.columns()[0],
        identity.columns()[1],
        identity.columns()[2],
        Vector::known([3.0, 4.0, 5.0, 0.0]),
    ]);
    let mut source = Source::new();
    source.control = [1.0, 0.0, 0.0, 0.0];
    source.cap = Ok(1.0);
    // This test pins the translation subtraction and the axis-angle sign;
    // with the native ClampIdeal default the 30° roll clamps the ideal back
    // to horizontal and the rotation is zero by design.
    source.clamp = false;
    try_update(
        HeadTrackingState {
            filter: &mut fixture.filter,
            target_position: &mut fixture.target,
            rotation: &mut fixture.rotation,
            rotation_velocity: &mut fixture.velocity,
            valid_target: &mut fixture.valid,
        },
        &identity,
        &head,
        &mut fixture.output,
        &mut source,
        &mut HostReckoningNumeric,
    )
    .unwrap();
    // Head Y turns thirty degrees toward +X: a negative Z rotation. The head
    // translation must be subtracted before constructing the orientation.
    assert!(fixture.output.xyz[0].abs() < 0.0001);
    assert!(fixture.output.xyz[1].abs() < 0.0001);
    assert!((fixture.output.xyz[2] + std::f32::consts::FRAC_PI_6).abs() < 0.002);
    assert_eq!(fixture.valid, Some(false));
}
