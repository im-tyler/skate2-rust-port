//! `ICEManager`'s playback camera for a NIS scene, over the ICE data
//! (`data\scene\ice_000.bin`, in `scene.big`).

use super::{
    directory::SceneProperties,
    manager::{CameraAnchors, CameraTrack, CameraTracks, IceFrame, PostfxChannels, SceneFrame, StartAudio, TrackUpdate},
};
use skate2_formats::ice_camera::{
    AVATARS, CameraSet, Mover, Playback, Spaces, avatar_orientation, element, make_ice_from_skate, scene_space,
};
use std::sync::Arc;

pub const PATH: &str = "data\\scene\\ice_000.bin";

/// `User_ICESceneManager::GetSceneContext` (`0x826cc968`): a scene type's ICE
/// context index (the context id is `0x80000003` plus it).
#[must_use]
pub fn context_index(kind: u8) -> u32 {
    match kind {
        1 => 3,
        2 => 4,
        3 | 27 => 5,
        4 => 6,
        5 | 6 => 7,
        7..=11 => 8,
        12 | 13 => 9,
        14 => 10,
        15 | 16 => 11,
        17 => 12,
        18 => 13,
        19..=21 => 14,
        23 => 16,
        24 => 17,
        25 => 18,
        26 => 19,
        _ => 15,
    }
}

/// `User_ICECameraAnchor::GetPosition` (`0x826cfc78`) hands the skater root
/// to ICE as (z, x, y); the connected scene is the active ICE scene.
/// `User_ICESceneManager::GetAvatarPosition` (`0x826d1608`), in the same
/// axes, answers avatar index 0 with the anchor, 1 with the replay camera's
/// translation (`CameraPresentation::GetReplayMatrix` `0x826cf400`), 3 with the
/// local skater's hips and 2 too, without a local skater the point 3 m along
/// the replay camera's Z row, 4 with scene entity 0 at zero height and from 5
/// with entity i - 5. An entity past the last is at the origin
/// (`Scene::GetEntityLocation` `0x82865490`).
/// `User_ICECameraAnchor::GetOrientation` (`0x826cd228`) gives the root's
/// axes through `MakeICEFromSkate`; `GetAvatarOrientation` (`0x826cf5f0`)
/// answers index 0 with the root's axes and 1 to 3 with the local skater's
/// `GetHipsMatrix` (1 and 2 with the replay camera's rotation without one),
/// each stood upright at the origin.
fn ice_spaces(scene: &SceneFrame, anchors: &CameraAnchors) -> Spaces {
    let ice = |[x, y, z]: [f32; 3]| [z, x, y];
    let entity = |i: usize| anchors.entities.get(i).copied().unwrap_or(Some([0.0; 3]));
    let replay_z = anchors.replay.map(|m| [m[8], m[9], m[10]]);
    let replay_position = anchors.replay.map(|m| [m[12], m[13], m[14]]);
    let ahead = replay_position.zip(replay_z).map(|(t, z)| std::array::from_fn(|lane| 3.0 * z[lane] + t[lane]));
    let avatars = std::array::from_fn::<_, AVATARS, _>(|i| {
        match i {
            0 => anchors.anchor,
            1 => replay_position,
            2 => anchors.hips.or(ahead),
            3 => anchors.hips,
            4 => entity(0).map(|[x, _, z]| [x, 0.0, z]),
            _ => entity(i - 5),
        }
        .map(ice)
    });
    let upright = |forward: [f32; 3]| avatar_orientation([[0.0; 3], [0.0; 3], forward, [0.0; 3]]);
    let mut avatar_orientations = [None; AVATARS];
    avatar_orientations[0] = anchors.axes.map(|[x, y, z]| avatar_orientation([x, y, z, [0.0; 3]]));
    avatar_orientations[1] = anchors.hips_forward.or(replay_z).map(upright);
    avatar_orientations[2] = avatar_orientations[1];
    avatar_orientations[3] = anchors.hips_forward.map(upright);
    Spaces {
        anchor: anchors.anchor.map(ice),
        anchor_orientation: anchors.axes.map(make_ice_from_skate),
        avatars,
        avatar_orientations,
        scene: Some(scene_space(scene.location, scene.orientation, scene.normal)),
    }
}

pub struct IceCameras {
    set: Arc<CameraSet>,
}

impl IceCameras {
    #[must_use]
    pub fn new(set: Arc<CameraSet>) -> Self {
        Self { set }
    }
}

impl CameraTracks for IceCameras {
    /// `ICEManager::ChooseCameraPlaybackTrack` for the scene's context group;
    /// `SetPlaybackCamera` sets the scene's time to `fSubParameter * subLength`
    /// when the take starts on a sub-take.
    fn connect(&self, scene: &SceneProperties, track: u8, length: Option<f32>) -> Option<(Box<dyn CameraTrack>, Option<f32>)> {
        let context = 0x8000_0003 + context_index(scene.kind);
        let playback = self.set.playback(context, scene.name_hash, usize::from(track))?;
        let set_time = (playback.int(element::CONTAINS_SUBTAKE) != 0)
            .then(|| playback.sub_parameter() * playback.sub_length(&self.set).unwrap_or(0.0));
        let track = IceTrack { set: Arc::clone(&self.set), playback, length, error: None, slow_motion_disabled: false };
        Some((Box::new(track), set_time))
    }

    /// `SceneCamera::GetListOfOverlays` (`0x82864f10`). A photo outro with
    /// fewer than 15 entries appends `Take_FixedPhoto` (85) when its first
    /// `PhotoShot` is at time 0 or absent, else `Take_NonFixedPhoto` (86),
    /// and always asks for the preload (`0x828650f0..0x82865150`).
    fn overlays(&self, scene: &SceneProperties, track: u8) -> Option<Vec<u8>> {
        let context = 0x8000_0003 + context_index(scene.kind);
        let mut list = self.set.overlays(context, scene.name_hash, usize::from(track));
        if scene.kind == super::directory::scene_type::PHOTO_OUTRO && list.as_ref().is_none_or(|l| l.len() < 15) {
            let time = self
                .set
                .playback(context, scene.name_hash, usize::from(track))
                .map_or(0.0, |p| p.time_at_photo_shot(&self.set, 0));
            list.get_or_insert_with(Vec::new).push(if time == 0.0 { 85 } else { 86 });
        }
        list
    }

    /// `NISManager::GetPhotoCount` (`0x828a0e70`): the scene's context group's assembly or
    /// take for the track, its freeze intervals whose command is `PhotoShot` (4).
    fn photo_count(&self, scene: &SceneProperties, track: u8) -> i32 {
        let context = 0x8000_0003 + context_index(scene.kind);
        self.set.playback(context, scene.name_hash, usize::from(track)).map_or(0, |playback| playback.photo_count(&self.set))
    }

    /// `SceneCamera::GetFixedFramePhotoCount` (`0x82864b98`): the same take's freeze intervals
    /// whose command is `PhotoShot_FixedFrame` (5).
    fn fixed_frame_photo_count(&self, scene: &SceneProperties, track: u8) -> i32 {
        let context = 0x8000_0003 + context_index(scene.kind);
        self.set.playback(context, scene.name_hash, usize::from(track)).map_or(0, |playback| playback.fixed_frame_photo_count(&self.set))
    }

    /// `ScenePlayer::AudioQueueForStart` (`0x82863cb8`): the scene's context
    /// group's assembly or take for the track (`GetAssembly`, else `GetTake`)
    /// set to parameter 0 (`ICETake::SetData(take, 0)`), and its post and
    /// speech values there.
    fn start_audio(&self, scene: &SceneProperties, track: u8) -> Option<StartAudio> {
        let context = 0x8000_0003 + context_index(scene.kind);
        let playback = self.set.playback(context, scene.name_hash, usize::from(track))?;
        let value = |id: usize| playback.int(id) as i32;
        Some(StartAudio {
            post_command: value(element::AUDIO_POST_CMD) as u32,
            post_id: value(element::AUDIO_POST_ID),
            speech_command: value(element::AUDIO_SPEECH_CMD) as u32,
            speech_id: value(element::AUDIO_SPEECH_ID),
            speech_character: value(element::AUDIO_SPEECH_CHARACTER),
        })
    }
}

struct IceTrack {
    set: Arc<CameraSet>,
    playback: Playback,
    /// The scene length `ConnectScene` writes over a replay-controlled scene's
    /// take length.
    length: Option<f32>,
    error: Option<String>,
    /// PC HoM replay policy; a new track defaults to its authored time scale.
    slow_motion_disabled: bool,
}

impl IceTrack {
    fn take_length(&self) -> f32 {
        self.length.unwrap_or_else(|| self.playback.length(&self.set))
    }
}

impl CameraTrack for IceTrack {
    /// `ICEManager::Update` (`0x82a3b5f0`) with an active scene.
    fn update(&mut self, elapsed: f32) -> TrackUpdate {
        if self.playback.assembly_interval().is_none() {
            let p = elapsed / self.take_length();
            self.playback.set_parameter(&self.set, p, false, false);
            return TrackUpdate { finished: p >= 1.0, set_time: None };
        }
        if self.playback.int(element::CONTAINS_SUBTAKE) == 0 {
            return TrackUpdate { finished: true, set_time: None };
        }
        let sub = self.playback.sub_length(&self.set).unwrap_or(0.0);
        let start = self.playback.assembly_interval_start(&self.set);
        let p = (sub / self.take_length()).mul_add(elapsed / sub - self.playback.float(element::TAKE_START), start);
        let p = if p <= 0.0 { 0.0 } else { p.min(1.0) };
        if !self.playback.set_parameter(&self.set, p, false, false) {
            return TrackUpdate::default();
        }
        // The camera mover resets (mSimTime 1, no hard cut, no render-object
        // lock) and the scene jumps to the new sub-take's time.
        let sub = self.playback.sub_length(&self.set).unwrap_or(0.0);
        TrackUpdate { finished: false, set_time: Some(self.playback.sub_parameter() * sub) }
    }

    fn sim_time(&self) -> f32 {
        if self.slow_motion_disabled {
            return 1.0;
        }
        let scale = self.playback.int(element::TIME_SCALE) as f32 * 0.01;
        if scale <= 0.01 {
            0.01
        } else if scale <= 1.0 {
            scale
        } else {
            1.0
        }
    }

    fn disable_slow_motion(&mut self) {
        self.slow_motion_disabled = true;
    }

    fn avoid_static_objects(&self) -> bool {
        self.playback.int(element::AVOID_STATIC_OBJECTS) != 0
    }

    fn camera(&mut self, scene: &SceneFrame, anchors: &CameraAnchors, mover: &mut Mover, dt: f32) -> Option<IceFrame> {
        match self.playback.camera(&self.set, &ice_spaces(scene, anchors), mover, self.sim_time() * dt, self.length) {
            Ok((matrix, fov)) => Some(IceFrame {
                matrix,
                fov,
                near_plane: self.playback.near_plane(),
                bloom: self.playback.bloom(),
                fade: self.playback.fade(),
                postfx: PostfxChannels::read(&self.playback),
            }),
            Err(e) => {
                self.error.get_or_insert(e);
                None
            }
        }
    }

    /// `Scene::Init` asks right after `ConnectScene`, before the playback
    /// advances: the take at parameter 0.
    fn time_zero(&self, scene: &SceneFrame, anchors: &CameraAnchors) -> Option<([f32; 3], [f32; 3])> {
        self.playback.eye_and_look(&ice_spaces(scene, anchors)).ok()
    }

    fn camera_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn overlay(&self) -> Option<i64> {
        self.playback.overlay(&self.set)
    }

    fn freeze_interval(&self) -> Option<usize> {
        self.playback.current_interval(element::FREEZE_CMD)
    }

    fn freeze_command(&self, interval: usize) -> (u32, u32) {
        let value = |id| self.playback.value_int_at(&self.set, id, interval) as u32;
        (value(element::FREEZE_CMD), value(element::FREEZE_TIME))
    }

    fn audio_effect_interval(&self) -> Option<usize> {
        self.playback.current_interval(element::AUDIO_EFFECT_CMD)
    }

    fn audio_effect_command(&self, interval: usize) -> (u32, u32) {
        let value = |id| self.playback.value_int_at(&self.set, id, interval) as u32;
        (value(element::AUDIO_EFFECT_CMD), value(element::AUDIO_EFFECT_ID))
    }

    fn audio_music_interval(&self) -> Option<usize> {
        self.playback.current_interval(element::AUDIO_MUSIC_CMD)
    }

    fn audio_music_command(&self, interval: usize) -> (u32, i32) {
        let value = |id| self.playback.value_int_at(&self.set, id, interval);
        (value(element::AUDIO_MUSIC_CMD) as u32, value(element::AUDIO_MUSIC_ID) as i32)
    }

    fn audio_speech_interval(&self) -> Option<usize> {
        self.playback.current_interval(element::AUDIO_SPEECH_CMD)
    }

    fn audio_speech_command(&self, interval: usize) -> (u32, i32, i32) {
        let value = |id| self.playback.value_int_at(&self.set, id, interval);
        (value(element::AUDIO_SPEECH_CMD) as u32, value(element::AUDIO_SPEECH_CHARACTER) as i32, value(element::AUDIO_SPEECH_ID) as i32)
    }

    fn audio_post_interval(&self) -> Option<usize> {
        self.playback.current_interval(element::AUDIO_POST_CMD)
    }

    fn audio_post_command(&self, interval: usize) -> (u32, i32) {
        let value = |id| self.playback.value_int_at(&self.set, id, interval);
        (value(element::AUDIO_POST_CMD) as u32, value(element::AUDIO_POST_ID) as i32)
    }

    fn parameter(&self) -> f32 {
        self.playback.parameter()
    }

    fn photo_shot_time(&self, index: usize) -> f32 {
        self.playback.photo_shot_parameter(&self.set, index).map_or(0.0, |parameter| self.take_length() * parameter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatars_follow_the_scene_managers_avatar_positions() {
        let anchors = CameraAnchors {
            anchor: Some([1.0, 2.0, 3.0]),
            axes: None,
            hips: Some([4.0, 5.0, 6.0]),
            entities: vec![Some([7.0, 8.0, 9.0]), None],
            ..CameraAnchors::default()
        };
        let spaces = ice_spaces(&SceneFrame::default(), &anchors);
        assert_eq!(spaces.anchor, Some([3.0, 1.0, 2.0]));
        assert_eq!(spaces.avatars[0], spaces.anchor);
        assert_eq!(spaces.avatars[1], None);
        let replay = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 10., 20., 30., 1.];
        let replay_spaces = ice_spaces(&SceneFrame::default(), &CameraAnchors { replay: Some(replay), hips: None, ..CameraAnchors::default() });
        assert_eq!(replay_spaces.avatars[1], Some([30.0, 10.0, 20.0]));
        assert_eq!(replay_spaces.avatars[2], Some([33.0, 10.0, 20.0]));
        assert!(replay_spaces.avatar_orientations[1].is_some() && replay_spaces.avatar_orientations[3].is_none());
        assert_eq!((spaces.avatars[2], spaces.avatars[3]), (Some([6.0, 4.0, 5.0]), Some([6.0, 4.0, 5.0])));
        // Entity 0 at zero height, then the entities: one without a render
        // entity unresolved, one past the last at the origin.
        assert_eq!(spaces.avatars[4], Some([9.0, 7.0, 0.0]));
        assert_eq!(spaces.avatars[5], Some([9.0, 7.0, 8.0]));
        assert_eq!(spaces.avatars[6], None);
        assert_eq!(spaces.avatars[7], Some([0.0; 3]));
    }

    /// replay_c_1a_05_besttrick_street_01 (NIS_BestTrick) looks at the skater
    /// in player space with LAG and BUNGEE: every frame resolves against the
    /// skater's root, the time-zero camera too, and the bungee closes on the
    /// skater as `fParameter` carries its weight toward 1.
    #[test]
    #[ignore = "requires original external game data via SKATE2_GAME_ROOT"]
    fn the_best_trick_replay_looks_at_the_skater_through_lag_and_bungee() {
        use crate::nis::directory::{scene_hash, scene_type};
        use skate2_formats::mount::{MountSet, MountedBig, SKATE2_ARCHIVES};
        let root = std::env::var("SKATE2_GAME_ROOT").expect("SKATE2_GAME_ROOT");
        let archive = SKATE2_ARCHIVES.iter().find(|a| a.relative_path == "data\\big\\scene.big").unwrap();
        let mut mounts = MountSet::new();
        mounts.push(MountedBig::open(archive.path_under(std::path::Path::new(&root)), archive.use_external_header).unwrap());
        let cameras = IceCameras::new(Arc::new(CameraSet::parse(&mounts.read_path(PATH).unwrap()).unwrap()));
        let props = SceneProperties {
            name_hash: scene_hash("replay_c_1a_05_besttrick_street_01"),
            kind: scene_type::BEST_TRICK,
            bit_field: 0,
            is_replay_on: 0,
            camera_track: 0,
            location: [0.0; 3],
            orientation: [0.0, 0.0, -1.0],
            length: 0.0,
            character_count: 0,
            character_start: 0,
            name: "replay_c_1a_05_besttrick_street_01".into(),
        };
        let (mut track, _) = cameras.connect(&props, 0, None).unwrap();
        // The skater faces Skate -z: ICE forward -x.
        let anchors = CameraAnchors {
            anchor: Some([100.0, 2.0, -50.0]),
            axes: Some([[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]]),
            hips: None,
            entities: Vec::new(),
            ..CameraAnchors::default()
        };
        let frame = SceneFrame::default();
        assert!(track.time_zero(&frame, &anchors).is_some());
        let mut mover = Mover::default();
        let (mut time, mut frames) = (0.0f32, 0);
        loop {
            if track.update(time).finished {
                break;
            }
            assert!(track.camera(&frame, &anchors, &mut mover, 1.0 / 30.0).is_some(), "{time}: {:?}", track.camera_error());
            frames += 1;
            time += 1.0 / 30.0;
        }
        assert!(frames > 30);
        let skater = [-50.0f32, 100.0, 2.0];
        let gap = (0..3).map(|c| (mover.bungee_position[c] - skater[c]).abs()).fold(0.0f32, f32::max);
        assert!(gap < 1e-3, "{:?} after {frames} frames", mover.bungee_position);
        eprintln!("forward {:?}, bungee forward {:?}", mover.forward.map(|c| c.val), mover.bungee_forward);
    }

    /// `GetListOfOverlays`' photo-outro append: photo_killed_m_1a_05_photo_rail
    /// shoots with `PhotoShot_FixedFrame` only (85), he_u2_photo01's first
    /// `PhotoShot` starts after time 0 (86).
    #[test]
    #[ignore = "requires original external game data via SKATE2_GAME_ROOT"]
    fn photo_outros_preload_their_photo_treatment() {
        use crate::nis::directory::{scene_hash, scene_type};
        use skate2_formats::mount::{MountSet, MountedBig, SKATE2_ARCHIVES};
        let root = std::env::var("SKATE2_GAME_ROOT").expect("SKATE2_GAME_ROOT");
        let archive = SKATE2_ARCHIVES.iter().find(|a| a.relative_path == "data\\big\\scene.big").unwrap();
        let mut mounts = MountSet::new();
        mounts.push(MountedBig::open(archive.path_under(std::path::Path::new(&root)), archive.use_external_header).unwrap());
        let cameras = IceCameras::new(Arc::new(CameraSet::parse(&mounts.read_path(PATH).unwrap()).unwrap()));
        let outro = |name: &str| SceneProperties {
            name_hash: scene_hash(name),
            kind: scene_type::PHOTO_OUTRO,
            bit_field: 0,
            is_replay_on: 0,
            camera_track: 0,
            location: [0.0; 3],
            orientation: [0.0, 0.0, -1.0],
            length: 0.0,
            character_count: 0,
            character_start: 0,
            name: name.into(),
        };
        let fixed = cameras.overlays(&outro("photo_killed_m_1a_05_photo_rail"), 0).unwrap();
        assert_eq!(fixed.last(), Some(&85), "{fixed:?}");
        let moving = cameras.overlays(&outro("he_u2_photo01"), 0).unwrap();
        assert_eq!(moving.last(), Some(&86), "{moving:?}");
    }

    /// `GetFixedFramePhotoCount` against `GetPhotoCount`: photo_killed_m_1a_05_photo_rail
    /// shoots eight `PhotoShot_FixedFrame` frames, photo_killed_m_1a_06_photo_puzzle eight
    /// `PhotoShot`s.
    #[test]
    #[ignore = "requires original external game data via SKATE2_GAME_ROOT"]
    fn photo_outros_count_their_fixed_frame_shots() {
        use crate::nis::directory::{scene_hash, scene_type};
        use skate2_formats::mount::{MountSet, MountedBig, SKATE2_ARCHIVES};
        let root = std::env::var("SKATE2_GAME_ROOT").expect("SKATE2_GAME_ROOT");
        let archive = SKATE2_ARCHIVES.iter().find(|a| a.relative_path == "data\\big\\scene.big").unwrap();
        let mut mounts = MountSet::new();
        mounts.push(MountedBig::open(archive.path_under(std::path::Path::new(&root)), archive.use_external_header).unwrap());
        let cameras = IceCameras::new(Arc::new(CameraSet::parse(&mounts.read_path(PATH).unwrap()).unwrap()));
        let outro = |name: &str| SceneProperties {
            name_hash: scene_hash(name),
            kind: scene_type::PHOTO_OUTRO,
            bit_field: 0,
            is_replay_on: 1,
            camera_track: 0,
            location: [0.0; 3],
            orientation: [0.0, 0.0, -1.0],
            length: 0.0,
            character_count: 0,
            character_start: 0,
            name: name.into(),
        };
        let rail = outro("photo_killed_m_1a_05_photo_rail");
        let puzzle = outro("photo_killed_m_1a_06_photo_puzzle");
        assert_eq!((cameras.fixed_frame_photo_count(&rail, 0), cameras.photo_count(&rail, 0)), (8, 0));
        assert_eq!((cameras.fixed_frame_photo_count(&puzzle, 0), cameras.photo_count(&puzzle, 0)), (0, 8));
    }

    /// photo_killed_m_1a_05_photo_rail's take opens on the photo treatment:
    /// the full fisheye (multiplier 1), contrast 2.5679 about 0.6602 far and
    /// 1 about 0.4843 near, a warm tint, the scanline roll from 0.9964 and
    /// the full radial blur; the grade and the blur end by about 2.1 s.
    #[test]
    #[ignore = "requires original external game data via SKATE2_GAME_ROOT"]
    fn a_photo_outro_opens_on_the_photo_treatment() {
        use crate::nis::directory::{scene_hash, scene_type};
        use skate2_formats::mount::{MountSet, MountedBig, SKATE2_ARCHIVES};
        let root = std::env::var("SKATE2_GAME_ROOT").expect("SKATE2_GAME_ROOT");
        let archive = SKATE2_ARCHIVES.iter().find(|a| a.relative_path == "data\\big\\scene.big").unwrap();
        let mut mounts = MountSet::new();
        mounts.push(MountedBig::open(archive.path_under(std::path::Path::new(&root)), archive.use_external_header).unwrap());
        let cameras = IceCameras::new(Arc::new(CameraSet::parse(&mounts.read_path(PATH).unwrap()).unwrap()));
        let name = "photo_killed_m_1a_05_photo_rail";
        let props = SceneProperties {
            name_hash: scene_hash(name),
            kind: scene_type::PHOTO_OUTRO,
            bit_field: 0,
            is_replay_on: 0,
            camera_track: 0,
            location: [0.0; 3],
            orientation: [0.0, 0.0, -1.0],
            length: 0.0,
            character_count: 0,
            character_start: 0,
            name: name.into(),
        };
        let (mut track, _) = cameras.connect(&props, 0, None).unwrap();
        let anchors = CameraAnchors { anchor: Some([0.0; 3]), axes: None, hips: Some([0.0; 3]), entities: Vec::new(), ..CameraAnchors::default() };
        let frame = SceneFrame::default();
        let mut mover = Mover::default();
        let mut camera = |time: f32| {
            track.update(time);
            track.camera(&frame, &anchors, &mut mover, 1.0 / 30.0).expect("camera").postfx
        };
        let open = camera(0.0);
        assert_eq!(open.fisheye, Some(f32::from_bits(0x4098_6186)));
        let [[near, near_mid], [far, far_mid]] = open.contrast.expect("contrast");
        assert!((near - 1.0).abs() < 1e-3 && (near_mid - 0.4843).abs() < 1e-3, "{near} {near_mid}");
        assert!((far - 2.5679).abs() < 1e-3 && (far_mid - 0.6602).abs() < 1e-3, "{far} {far_mid}");
        assert!(open.tint.expect("tint").iter().all(|[r, g, b]| *r == 1.0 && r >= g && g >= b));
        assert!((open.scanline.expect("scanline") - 0.9964).abs() < 1e-3);
        assert_eq!(open.radial_blur, Some(1.0));
        assert!(open.saturate.is_some() && open.tonemap.is_none() && open.near_far.is_none() && open.brightness.is_none());
        let settled = camera(2.4);
        assert_eq!(settled.scanline, Some(0.0));
        assert!(settled.contrast.is_none() && settled.tint.is_none() && settled.radial_blur.is_none() && settled.fisheye.is_some());
    }
}
