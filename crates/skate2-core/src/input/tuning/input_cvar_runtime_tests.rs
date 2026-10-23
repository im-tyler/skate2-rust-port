use super::*;

#[test]
fn camera_obstacle_cvars_link_source_defaults_and_follow_live_startup_values() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_pedestrian_avoidance(),
        Err(InputCvarError::Unlinked("kbPedestrianAvoidance"))
    );
    assert_eq!(
        refs.try_actor_search_radius(),
        Err(InputCvarError::Unlinked("kActorSearchRadius"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_pedestrian_avoidance().unwrap());
    assert!(refs.try_skater_avoidance().unwrap());
    assert_eq!(
        refs.try_actor_search_radius().unwrap().to_bits(),
        10f32.to_bits()
    );
    assert!(refs.try_interleave_avoidance().unwrap());
    assert!(refs.try_enable_collision_avoidance().unwrap());
    assert!(!refs.try_show_avoidance_debug().unwrap());
    assert_eq!(
        refs.try_insignificant_speed().unwrap().to_bits(),
        0x3e19_999a
    );

    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkbPedestrianAvoidance=0\r\nkbSkaterAvoidance=0\r\nkActorSearchRadius=20\r\nkbInterleaveAvoidance=0\r\nkbEnableCollisionAvoidance=0\r\nkbShowAvoidanceDebug=1\r\nkInsignificantSpeed=0.3\r\n"))
        .unwrap();
    assert!(!refs.try_pedestrian_avoidance().unwrap());
    assert!(!refs.try_skater_avoidance().unwrap());
    assert_eq!(
        refs.try_actor_search_radius().unwrap().to_bits(),
        20f32.to_bits()
    );
    assert!(!refs.try_interleave_avoidance().unwrap());
    assert!(!refs.try_enable_collision_avoidance().unwrap());
    assert!(refs.try_show_avoidance_debug().unwrap());
    assert_eq!(
        refs.try_insignificant_speed().unwrap().to_bits(),
        0.3f32.to_bits()
    );
}

#[test]
fn offboard_entry_speed_limit_uses_live_linked_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_ob_max_landing_speed_for_blend(),
        Err(InputCvarError::Unlinked("kOB_MaxLandingSpeedForBlend"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_ob_max_landing_speed_for_blend().unwrap().to_bits(),
        10.0f32.to_bits()
    );
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkOB_MaxLandingSpeedForBlend=7.25\r\n"))
        .unwrap();
    assert_eq!(
        refs.try_ob_max_landing_speed_for_blend().unwrap().to_bits(),
        7.25f32.to_bits()
    );
}

#[test]
fn forced_surface_uses_real_linked_enum_and_live_reloads() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_forced_world_surface(),
        Err(InputCvarError::Unlinked("kForcedWorldSurface"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_forced_world_surface().unwrap(), 0);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkForcedWorldSurface=7\r\n"))
        .unwrap();
    assert_eq!(refs.try_forced_world_surface().unwrap(), 7);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkForcedWorldSurface=2\r\n"))
        .unwrap();
    assert_eq!(refs.try_forced_world_surface().unwrap(), 2);
    // ReadLabel82a8a7ac calls GetInt for enums; display tags are not INI values.
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkForcedWorldSurface=rough\r\n"))
        .unwrap();
    assert_eq!(refs.try_forced_world_surface().unwrap(), 0);
}

#[test]
fn board_direction_debug_references_require_link_and_follow_live_rebinding() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_skater_look_direction(),
        Err(InputCvarError::Unlinked("kSkaterLookDirection"))
    );
    assert_eq!(
        refs.try_skateboard_vel_direction(),
        Err(InputCvarError::Unlinked("kSkateboardVelDirection"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_skater_look_direction().unwrap());
    assert!(!refs.try_skateboard_vel_direction().unwrap());
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkSkaterLookDirection=1\r\nkSkateboardVelDirection=1\r\n",
        ))
        .unwrap();
    assert!(refs.try_skater_look_direction().unwrap());
    assert!(refs.try_skateboard_vel_direction().unwrap());
    InputCvarManager::new().try_link(&refs).unwrap();
    assert!(!refs.try_skater_look_direction().unwrap());
    assert!(!refs.try_skateboard_vel_direction().unwrap());
}

#[test]
fn feet_radius_and_fade_times_remain_live_unclamped_startup_values() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_feet_line_test_radius(),
        Err(InputCvarError::Unlinked("kFeetLineTestRadius"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_feet_line_test_radius().unwrap().to_bits(),
        0x3cf5_c28f
    );
    assert_eq!(
        refs.try_skater_fade_in_time().unwrap().to_bits(),
        0x3f80_0000
    );
    assert_eq!(
        refs.try_skater_fade_out_time().unwrap().to_bits(),
        0x3f80_0000
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkFeetLineTestRadius=-2\r\nkSkaterFadeInTime=0\r\nkSkaterFadeOutTime=9\r\n")).unwrap();
    assert_eq!(
        refs.try_feet_line_test_radius().unwrap().to_bits(),
        (-2.0_f32).to_bits()
    );
    assert_eq!(refs.try_skater_fade_in_time().unwrap().to_bits(), 0);
    assert_eq!(
        refs.try_skater_fade_out_time().unwrap().to_bits(),
        9.0_f32.to_bits()
    );
}

#[test]
fn wipeout_condition_references_sample_live_targets_after_relink() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_debug_wipeout_conditions(),
        Err(InputCvarError::Unlinked("kDebugWipeoutConditions"))
    );
    assert_eq!(
        refs.try_time_to_land_to_wipeout(),
        Err(InputCvarError::Unlinked("kTimeToLandToWipeout"))
    );
    assert_eq!(
        refs.try_min_time_in_air_to_wipeout(),
        Err(InputCvarError::Unlinked("kMinTimeInAirToWipeOut"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_debug_wipeout_conditions().unwrap());
    assert_eq!(
        refs.try_time_to_land_to_wipeout().unwrap().to_bits(),
        0x3fb3_3333
    );
    assert_eq!(
        refs.try_min_time_in_air_to_wipeout().unwrap().to_bits(),
        0x3f4c_cccd
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkDebugWipeoutConditions=1\r\nkTimeToLandToWipeout=8\r\nkMinTimeInAirToWipeOut=-2\r\n")).unwrap();
    assert!(refs.try_debug_wipeout_conditions().unwrap());
    assert_eq!(
        refs.try_time_to_land_to_wipeout().unwrap().to_bits(),
        8.0_f32.to_bits()
    );
    assert_eq!(
        refs.try_min_time_in_air_to_wipeout().unwrap().to_bits(),
        (-2.0_f32).to_bits()
    );
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert!(!refs.try_debug_wipeout_conditions().unwrap());
    assert_eq!(
        refs.try_time_to_land_to_wipeout().unwrap().to_bits(),
        0x3fb3_3333
    );
    assert_eq!(
        refs.try_min_time_in_air_to_wipeout().unwrap().to_bits(),
        0x3f4c_cccd
    );
}

#[test]
fn debug_animations_condition_reads_linked_and_relinked_cvar() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_use_debug_animations_mode(),
        Err(InputCvarError::Unlinked("kUseDebugAnimationsMode"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_use_debug_animations_mode().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkUseDebugAnimationsMode=1\r\n"))
        .unwrap();
    assert!(refs.try_use_debug_animations_mode().unwrap());
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert!(!refs.try_use_debug_animations_mode().unwrap());
}

#[test]
fn grind_balance_reads_each_current_target_and_preserves_counter_prefix() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_lean_min_non_grind_frames(),
        Err(InputCvarError::Unlinked("kLeanMinNonGrindFrames"))
    );
    assert_eq!(
        refs.try_curb_exit_start_time(),
        Err(InputCvarError::Unlinked("kCurbExitStartTime"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_lean_min_non_grind_frames().unwrap(), 20);
    assert_eq!(
        refs.try_curb_exit_start_time().unwrap().to_bits(),
        2f32.to_bits()
    );
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkLeanMinNonGrindFrames=2147483647\r\nkCurbExitStartTime=8\r\n",
        ))
        .unwrap();
    let retained_counter = refs
        .try_lean_min_non_grind_frames()
        .unwrap()
        .wrapping_add(1);
    assert_eq!(retained_counter, i32::MIN);
    // Relink occurs after the original counter store but before the source's
    // second target load; the curb getter must read the new manager's target.
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_curb_exit_start_time().unwrap().to_bits(),
        2f32.to_bits()
    );
    assert_eq!(retained_counter, i32::MIN);
    relinked
        .try_deserialize_startup(Some(b"[Config]\r\nkCurbExitStartTime=8\r\n"))
        .unwrap();
    assert_eq!(
        refs.try_curb_exit_start_time().unwrap().to_bits(),
        8f32.to_bits()
    );
    assert_eq!(refs.try_lean_min_non_grind_frames().unwrap(), 20);
}

#[test]
fn motion_angle_default_reads_live_cvar_without_gui_clamping() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_mo_angle_max_delta(),
        Err(InputCvarError::Unlinked("kMOAngleMaxDelta"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_mo_angle_max_delta().unwrap().to_bits(),
        2f32.to_bits()
    );
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkMOAngleMaxDelta=120\r\n"))
        .unwrap();
    assert_eq!(
        refs.try_mo_angle_max_delta().unwrap().to_bits(),
        120f32.to_bits()
    );
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_mo_angle_max_delta().unwrap().to_bits(),
        2f32.to_bits()
    );
}

#[test]
fn wipeout_velocity_reads_follow_live_targets_and_preserve_unclamped_values() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_up_velocity_scale_lower(),
        Err(InputCvarError::Unlinked("kUpVelocityScaleLower"))
    );
    assert_eq!(
        refs.try_up_velocity_scale_upper(),
        Err(InputCvarError::Unlinked("kUpVelocityScaleUpper"))
    );
    assert_eq!(
        refs.try_max_skeleton_part_velocity(),
        Err(InputCvarError::Unlinked("kMaxSkeletonPartVelocity"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_up_velocity_scale_lower().unwrap().to_bits(), 0);
    assert_eq!(
        refs.try_up_velocity_scale_upper().unwrap().to_bits(),
        0x3dcc_cccd
    );
    assert_eq!(
        refs.try_max_skeleton_part_velocity().unwrap().to_bits(),
        20f32.to_bits()
    );
    assert_eq!(
        refs.try_wipeout_up_velocity_scales().unwrap(),
        (f32::from_bits(0x3dcc_cccd), 0.0)
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkUpVelocityScaleLower=-2\r\nkUpVelocityScaleUpper=3\r\nkMaxSkeletonPartVelocity=150\r\n")).unwrap();
    assert_eq!(
        refs.try_up_velocity_scale_lower().unwrap().to_bits(),
        (-2f32).to_bits()
    );
    assert_eq!(
        refs.try_up_velocity_scale_upper().unwrap().to_bits(),
        3f32.to_bits()
    );
    assert_eq!(
        refs.try_max_skeleton_part_velocity().unwrap().to_bits(),
        150f32.to_bits()
    );
    assert_eq!(refs.try_wipeout_up_velocity_scales().unwrap(), (3.0, -2.0));
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert_eq!(refs.try_up_velocity_scale_lower().unwrap().to_bits(), 0);
    assert_eq!(
        refs.try_up_velocity_scale_upper().unwrap().to_bits(),
        0x3dcc_cccd
    );
    assert_eq!(
        refs.try_max_skeleton_part_velocity().unwrap().to_bits(),
        20f32.to_bits()
    );
}

#[test]
fn grind_approach_constructor_reads_relinked_current_integer_without_gui_clamping() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_min_approach_non_grind_frames(),
        Err(InputCvarError::Unlinked("kMinApproachNonGrindFrames"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_min_approach_non_grind_frames().unwrap(), 30);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkMinApproachNonGrindFrames=71\r\n"))
        .unwrap();
    assert_eq!(refs.try_min_approach_non_grind_frames().unwrap(), 71);
    let mut relinked = InputCvarManager::new();
    relinked.try_link(&refs).unwrap();
    assert_eq!(refs.try_min_approach_non_grind_frames().unwrap(), 30);
}

#[test]
fn air_drive_reads_follow_relinked_live_targets_without_gui_range_clamping() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_skateboard_soft_drive_disp(),
        Err(InputCvarError::Unlinked("kSkateboardSoftDriveDisp"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_skateboard_soft_drive_disp().unwrap().to_bits(),
        20000_f32.to_bits()
    );
    assert_eq!(
        refs.try_skateboard_soft_drive_strength().unwrap().to_bits(),
        10_f32.to_bits()
    );
    assert!(!refs.try_draw_physics_air_debug_info().unwrap());
    first.try_deserialize_startup(Some(b"[Config]\r\nkSkateboardSoftDriveDisp=-2\r\nkSkateboardSoftDriveStr=125\r\nkbDrawPhysicsAirDebugInfo=1\r\n")).unwrap();
    assert_eq!(
        refs.try_skateboard_soft_drive_disp().unwrap().to_bits(),
        (-2_f32).to_bits()
    );
    assert_eq!(
        refs.try_skateboard_soft_drive_strength().unwrap().to_bits(),
        125_f32.to_bits()
    );
    assert!(refs.try_draw_physics_air_debug_info().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkSkateboardSoftDriveDisp=7\r\n"))
        .unwrap();
    assert_eq!(
        refs.try_skateboard_soft_drive_disp().unwrap().to_bits(),
        20000_f32.to_bits()
    );
    assert_eq!(
        refs.try_skateboard_soft_drive_strength().unwrap().to_bits(),
        10_f32.to_bits()
    );
    assert!(!refs.try_draw_physics_air_debug_info().unwrap());
}

#[test]
fn blend_cull_reference_selection_reads_only_the_selected_current_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_skater_blend_cull_threshold(true),
        Err(InputCvarError::Unlinked("kSkaterLowBlendCullThresh"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(
        refs.try_skater_blend_cull_threshold(true)
            .unwrap()
            .to_bits(),
        0x3c23_d70a
    );
    assert_eq!(
        refs.try_skater_blend_cull_threshold(false)
            .unwrap()
            .to_bits(),
        0x3dcc_cccd
    );
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkSkaterLowBlendCullThresh=0.2\r\nkSkaterHighBlendCullThresh=0.4\r\n",
        ))
        .unwrap();
    assert_eq!(
        refs.try_skater_blend_cull_threshold(true)
            .unwrap()
            .to_bits(),
        0.2f32.to_bits()
    );
    assert_eq!(
        refs.try_skater_blend_cull_threshold(false)
            .unwrap()
            .to_bits(),
        0.4f32.to_bits()
    );
    refs.targets.lock().unwrap()[27] = None;
    assert_eq!(
        refs.try_skater_blend_cull_threshold(true)
            .unwrap()
            .to_bits(),
        0.2f32.to_bits()
    );
    assert_eq!(
        refs.try_skater_blend_cull_threshold(false),
        Err(InputCvarError::Unlinked("kSkaterHighBlendCullThresh"))
    );
}

#[test]
fn ragdoll_references_read_live_targets_and_rebind_to_source_defaults() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_use_ragdoll_joint_limits(),
        Err(InputCvarError::Unlinked(_))
    ));
    assert!(matches!(
        refs.try_bouncy_ragdoll_cheat(),
        Err(InputCvarError::Unlinked(_))
    ));
    assert!(matches!(
        refs.try_bouncy_ragdoll_restitution(),
        Err(InputCvarError::Unlinked(_))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_use_ragdoll_joint_limits().unwrap());
    assert!(!refs.try_bouncy_ragdoll_cheat().unwrap());
    assert_eq!(
        refs.try_bouncy_ragdoll_restitution().unwrap().to_bits(),
        0x3f7d_70a4
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkSB_UseRagdollJointLimits=0\r\nkBouncyRagdollCheat=1\r\nkBouncyRagdollCheatRestitution=2\r\n")).unwrap();
    assert!(!refs.try_use_ragdoll_joint_limits().unwrap());
    assert!(refs.try_bouncy_ragdoll_cheat().unwrap());
    assert_eq!(
        refs.try_bouncy_ragdoll_restitution().unwrap().to_bits(),
        2f32.to_bits()
    );
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkBouncyRagdollCheatRestitution=-3\r\n"))
        .unwrap();
    assert_eq!(
        refs.try_bouncy_ragdoll_restitution().unwrap().to_bits(),
        (-3f32).to_bits()
    );
    InputCvarManager::new().try_link(&refs).unwrap();
    assert!(refs.try_use_ragdoll_joint_limits().unwrap());
    assert!(!refs.try_bouncy_ragdoll_cheat().unwrap());
    assert_eq!(
        refs.try_bouncy_ragdoll_restitution().unwrap().to_bits(),
        0x3f7d_70a4
    );
}
use ActionInputBoolCvar::{
    UseReleaseForWipeout, UseSprintButtonInput, UseTriggersForExplicitWipeout,
};
use ActionInputFloatCvar::{NewPushAllowance, NewToggleOffboardAllowance, TimeToConsiderWipeout};

#[test]
fn danger_zone_reference_requires_link_and_rereads_live_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_show_danger_zone(),
        Err(InputCvarError::Unlinked("kShowDangerZone"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_show_danger_zone().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkShowDangerZone=1\r\n"))
        .unwrap();
    assert!(refs.try_show_danger_zone().unwrap());
}

#[test]
fn static_refs_remain_null_until_manager_links_exact_initializers() {
    let refs = InputCvarReferences::new_static_initialized();
    let mut provider = refs.fill_provider();
    assert_eq!(
        provider.capture_float(NewPushAllowance),
        Err(InputCvarError::Unlinked("kNewPushAllowance"))
    );
    assert_eq!(
        provider.boolean(UseSprintButtonInput),
        Err(InputCvarError::Uncaptured("kUseSprintButtonInput"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    for (key, bits) in [
        (NewPushAllowance, 0x3e99_999a),
        (TimeToConsiderWipeout, 0x3e4c_cccd),
        (NewToggleOffboardAllowance, 0x3cf5_c28f),
    ] {
        provider.capture_float(key).unwrap();
        assert_eq!(provider.float(key).unwrap().to_bits(), bits);
    }
    for (key, expected) in [
        (UseSprintButtonInput, true),
        (UseTriggersForExplicitWipeout, true),
        (UseReleaseForWipeout, false),
    ] {
        provider.capture_boolean(key).unwrap();
        assert_eq!(provider.boolean(key).unwrap(), expected);
    }
}
#[test]
fn captured_targets_reread_mutation_survive_reference_rebind_and_relink_does_not_reset() {
    let refs = InputCvarReferences::new_static_initialized();
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    let mut provider = refs.fill_provider();
    provider.capture_float(NewPushAllowance).unwrap();
    provider.capture_boolean(UseSprintButtonInput).unwrap();
    first
        .try_deserialize_startup(Some(
            b"[Config]\r\nkNewPushAllowance=2.5\r\nkUseSprintButtonInput=0\r\n",
        ))
        .unwrap();
    assert_eq!(
        provider.float(NewPushAllowance).unwrap().to_bits(),
        2.5f32.to_bits()
    );
    assert!(!provider.boolean(UseSprintButtonInput).unwrap());
    first.try_link(&refs).unwrap();
    assert_eq!(
        provider.float(NewPushAllowance).unwrap().to_bits(),
        2.5f32.to_bits()
    );
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    second
        .try_deserialize_startup(Some(b"[Config]\r\nkNewPushAllowance=-4\r\n"))
        .unwrap();
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkNewPushAllowance=7\r\n"))
        .unwrap();
    assert_eq!(
        provider.float(NewPushAllowance).unwrap().to_bits(),
        7f32.to_bits()
    );
    provider.capture_float(NewPushAllowance).unwrap();
    assert_eq!(
        provider.float(NewPushAllowance).unwrap().to_bits(),
        (-4f32).to_bits()
    );
    // Another static reference occurrence links to the retained label, not a default copy.
    let other = InputCvarReferences::new_static_initialized();
    second.try_link(&other).unwrap();
    let mut other_provider = other.fill_provider();
    other_provider.capture_float(NewPushAllowance).unwrap();
    assert_eq!(
        other_provider.float(NewPushAllowance).unwrap().to_bits(),
        (-4f32).to_bits()
    );
}
#[test]
fn ini_preserves_first_duplicate_case_spaces_and_empty_line_termination() {
    let values = ini::parse(b"[Config]\r\na=1\r\na=2\r\nA=3\r\n a =4 \r\n\r\nb=5\r\n[Other]\r\na=6\r\n[Config]\r\nc=7\n\na=8\nd=9").unwrap();
    assert_eq!(values.get(b"a".as_slice()).unwrap(), b"1");
    assert_eq!(values.get(b"A".as_slice()).unwrap(), b"3");
    assert_eq!(values.get(b"a ".as_slice()).unwrap(), b"4");
    assert_eq!(values.get(b"b".as_slice()).unwrap(), b"5");
    assert_eq!(values.get(b"c".as_slice()).unwrap(), b"7");
    assert!(!values.contains_key(b"d".as_slice()));
}
#[test]
fn custom_converters_distinguish_standard_parse_and_retain_signed_zero() {
    assert_eq!(ini::integer(b"true"), 0);
    assert_eq!(ini::integer(b"prefix+12tail"), 12);
    assert_eq!(ini::integer(b"prefix-0x12"), 0);
    assert_eq!(ini::integer(b"4294967295"), -1);
    assert_eq!(ini::float(b"foo1-2").to_bits(), (-12f32).to_bits());
    assert_eq!(ini::float(b"1.2.3").to_bits(), (123f32 / 100f32).to_bits());
    assert_eq!(ini::float(b"5e+2").to_bits(), 5f32.to_bits());
    assert_eq!(ini::float(b"-0").to_bits(), (-0f32).to_bits());
    assert_eq!(
        ini::float(b"3e-5").to_bits(),
        ((3f32 * f32::from_bits(0x38d1_b717)) * f32::from_bits(0x3dcc_cccd)).to_bits()
    );
    assert_eq!(ini::float(b"NaN").to_bits(), 0);
}

/// True when every `recorded` label appears in `actual`, in order: the
/// hand-maintained title-order list pins membership and ordering without
/// copying the whole linked set (which grows with every added cvar).
fn covers_in_order(actual: &[&str], recorded: &[&str]) -> bool {
    let mut remaining = actual;
    recorded.iter().all(
        |label| match remaining.iter().position(|entry| entry == label) {
            Some(at) => {
                remaining = &remaining[at + 1..];
                true
            }
            None => false,
        },
    )
}
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one original cvar title-order and missing-config preservation oracle"
)]
fn missing_load_and_missing_keys_preserve_targets_and_source_title_order() {
    let refs = InputCvarReferences::new_static_initialized();
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    let actual: Vec<_> = manager
        .labels
        .iter()
        .map(|label| label.definition.label)
        .collect();
    // The hand-maintained list is asserted as an ordered subsequence of the
    // linked labels: it pins that these cvars appear, in title order, without
    // copying every later addition verbatim.
    let recorded_is_subsequence = covers_in_order(
        &actual,
        &[
            "kFeedDMOGrindSplines",
            "kMaxDynamicObjectSize",
            "kMaxVechileObjectSize",
            "cv_EnableBlendspaceJobs",
            "cv_EnableBlendspacePrint",
            "cv_NoFaceAnimation",
            "cv_NoHandAnimation",
            "cv_IndividualFullbindpose",
            "kShowDangerZone",
            "kbSlideDebugDisplay",
            "kUseJobsForSkaterAnimUpdate",
            "kUseSlicingProjection3",
            "kUseSlicingProjectionN",
            "kUseSprintButtonInput",
            "kUseVBR",
            "kUseBlendSpaceOptimizations",
            "kbDrawTrickHeightDebugInfo",
            "kSkaterHighBlendCullThresh",
            "kSkaterLowBlendCullThresh",
            "kAnimDownGesture",
            "kEnableDebugGestures",
            "kAnimLeftGesture",
            "kAnimRightGesture",
            "kAnimUpGesture",
            "kAutoJumpTresh",
            "kLocomotionStartYOffset",
            "kBezierInfluencePercentage",
            "kDisplayLocomotionDebug",
            "kEnableLocomotionFutureAssert",
            "kEnableJumpHeightAdjust",
            "kLocColl_DebugDraw",
            "kTwoFootLandingImpactThreshold",
            "kInclineSlopeTresh",
            "kInclineAngle",
            "kInclineMaxSlopeDown",
            "kInclineMaxSlopeUp",
            "kJumpHeight",
            "kLand1FTTresh",
            "kLand2FTTresh",
            "kMaxAccel",
            "kMaxDecel",
            "kMaxDecelStop",
            "kSkaterAnimMaxRawTime",
            "kLocomotionMinApexHeight",
            "kShoeOffset",
            "kSkel_SpeedScalarOnEnterLoc",
            "kStairsSlopeTresh",
            "kUseSprintButton",
            "kLocoWipeoutDelta",
            "zInclineTresh",
            "zStairsTresh",
            "k2ndDegreEqRatio",
            "kCorrectionAngleSign",
            "kCorrectionTurnSign",
            "kDistTresh4Oscil",
            "kJumpStairsTresh",
            "kMaxDecelStepRatio",
            "kMinTimeInAirToWipeOut",
            "kTimeRatio",
            "kTimeToLandToWipeout",
            "kAdditionalAngleAway",
            "kOBCOLL_DisplacementBlend",
            "kALC_HardDriveDisp",
            "kALC_HardDriveStr",
            "kJumpMaxStepHeight",
            "kOBCOLL_MaxAutoStartTurnAngle",
            "kOBCOLL_MaxAutoStartTurnAngleAir",
            "kMaxFramesForStepSelectorToFail",
            "kOBCOLL_MaxTurnAngleDegs",
            "kOB_MaxCollisionDisp",
            "kOBCOLL_MinDisp",
            "kOffboardDriveScalar",
            "kRunMaxStepHeight",
            "kOBCOLL_DebugSpreadAngleMax",
            "kOBCOLL_DebugSpreadAngleMin",
            "kMaxTimeToAvoidCollision",
            "kSA_UseWorldYForNormal",
            "kDebugLocoCommonConditions",
            "kDebugLocoMotionConditions",
            "kMOAngleMaxDelta",
            "kMOCrouchMaxDelta",
            "kMORawSpeedMax",
            "kMOSpeedMaxDelta",
            "kMOStrengthMaxDelta",
            "kAllowHolding",
            "kCapDvAtTopSpeed",
            "kUseDebugAnimationsMode",
            "kGestureMappingDebug",
            "kMongoPushToAnticThresh",
            "kUseBump",
            "kSetTilt",
            "kTiltX",
            "kUseTilt",
            "kUseAngleMap",
            "kUseAngleOffset",
            "kUseMagMap",
            "kLegAdjustCustomPreLandHack",
            "kShowSkaterInAirDebugText",
            "kEnableLandingOnBoardBodySpin",
            "kLegAdjustDown",
            "kLegAdjustPreLand",
            "kLegAdjustUp",
            "kLandingImpactMaxBlendDelta",
            "kLandingImpactMinBlendDelta",
            "kLandingLandPoseBlendDelta",
            "kLandingTimeToBlendToLandPose",
            "kLandingTimeToBlendToTwoFoot",
            "kLandingTimeToWaitToBlendToTwoFoot",
            "kLandingVelocityTwoFootThreshold",
            "kOffBoardAirPrelandTime",
            "kLegAdjustUseCustomPreLandHack",
            "kUsePumping",
            "kSlowMotionScale",
            "kDebugWipeoutConditions",
            "kbEnableNavMeshConstraints",
            "kEnableWipeoutSpotBreadCrumb",
            "kfInsignificantSquareVelocity",
            "kMinTicksAfterResetForBreadCrumb",
            "kbHeavyweightHardCutForAllCameras",
            "kActorSearchRadius",
            "kbAvoidanceElevationEnabled",
            "kbEnableCollisionAvoidance",
            "kbInterleaveAvoidance",
            "kbPedestrianAvoidance",
            "kbSkaterAvoidance",
            "kbEnableAirCameraCollision",
            "kbLockShot",
            "kMaxPositionRetries",
            "kbNPCThreatTest",
            "kbShotDatabaseLiveChanges",
            "kbShowAirCollision",
            "kbShowAnchorDebug",
            "kbShowAvoidanceDebug",
            "kbShowFrontsideAngle",
            "kbShowMirroring",
            "kbShowAvoidancePosition",
            "kbShowIntendedPosition",
            "kbShowTrackerDebug",
            "kbShowTrajectoryInfo",
            "kbShowTransferAngleDebug",
            "kbShowTurning",
            "kStandingOffset",
            "kbDropTestRadius",
            "kbEnableGrindDetector",
            "kNumSimCameras",
            "kbElevationAccClamp",
            "kbElevationSmoothing",
            "kbElevationSpeedClamp",
            "kbHeadingAccClamp",
            "kbHeadingSmoothing",
            "kbHeadingSpeedClamp",
            "kbPitchAccClamp",
            "kbPitchSmoothing",
            "kbPitchSpeedClamp",
            "kbYawAccClamp",
            "kbYawSmoothing",
            "kbYawSpeedClamp",
            "kInsignificantSpeed",
            "kBouncyBigBlackEnabled",
            "kUseReleaseForWipeout",
            "kUseAutoPump",
            "kNewPushAllowance",
            "kNewToggleOffboardAllowance",
            "kTimeToConsiderWipeout",
            "kUseTriggersForExplicitWipeout",
            "kDistComToBoard",
            "kKickTurnSmoothing",
            "kEnableJuiceConnectionRetry",
            "kbEnableGhostByDefault",
            "kSchemaLoadFromBinary",
            "kUseFineKickturn",
            "kGroundStairsFrames",
            "kDebugDrawNearbyGrindPrimatives",
            "kGrindableDetectionRadius",
            "kDebugRenderLocalSimPlayer",
            "kInputHistoryDeadZone",
            "kDisplayLeftStickGestures",
            "kDisplayLeftStickInput",
            "kDisplayRightStickGestures",
            "kDisplayRightStickInput",
            "kFastestGestureRatio",
            "kMaxError",
            "kMinError",
            "kSlowestGestureRatio",
            "kHighFreqOffset",
            "kHighFreqScale",
            "kLowFreqOffset",
            "kLowFreqScale",
            "kMaxGoodVelY",
            "kMinBadVelY",
            "kOrthonalTolerance",
            "kWipeoutOnCollision",
            "kDrawCarveInfo",
            "kDrawTiltInfo",
            "kUseXYZCentreOfMass",
            "kActivateSpeedCheat",
            "kSKC_GrabCheat",
            "kBouncyRagdollCheat",
            "kBouncyRagdollCheatRestitution",
            "kWipeoutDisableAllWipeouts",
            "kSpeedCheatSpeed",
            "kMinApproachNonGrindFrames",
            "kMaxStairTrickDeckNoseUp",
            "kMinStairTrickSpeed",
            "kDrawSpeedWobbleInfo",
            "kbEnableAdjustSpawnPosition",
            "kRigidRagdoll",
            "kSkaterLookDirection",
            "kSkateboardVelDirection",
            "kDeckFlickerScore",
            "kStateFlickerAvoidGrindFrames",
            "kTooMuchFlickerThresh",
            "kTruckFlickerScore",
            "kEnableUsualGrinds",
            "kCurbExitStartTime",
            "kLeanMinNonGrindFrames",
            "kApplyFakeTorque",
            "kFakeTorqueExponent",
            "kFakeTorqueValue",
            "kSpewJumpStuff",
            "kBodySpinThresh",
            "kForceSpinThresh",
            "kHiBodySpin",
            "kHiSideSpeed",
            "kLoBodySpin",
            "kLoSideSpeed",
            "kStraightAngleThresh",
            "kStraightSpeedThresh",
            "kShowManualInfo",
            "kShowVelRemoval",
            "kWrongWheelScalar",
            "kManualForceMaxFramesSpinningTheSameWay",
            "kMaxOnlineIterations",
            "kMaxSinglePlayerIterations",
            "kbDisplayBadTriangleInfo",
            "kAdditionalGapFromVertTri",
            "kDistToDeathJumpDown",
            "kGeomAnalyzerMinBadY",
            "kVertMinYTriangleNormal",
            "kSSNormYThreshold",
            "kSSStepRadius",
            "kSSTestHeight",
            "kSSTestRadius",
            "kSSTrajMaxTime",
            "kSSTrajectoryRadius",
            "kAcceptableDistFromDest",
            "kCosSpreadAngle",
            "kGrindBoxHalfSize",
            "kGrindLowStepHeight",
            "kGrindVerticalSinAngle",
            "kGrindAddedDistTest",
            "kHeightMargin",
            "kMinDistToSamePoint",
            "kNbTrajWhenSprinting",
            "kReduceSpeedAcceptRatio",
            "kDampedComBlend",
            "kDampedComBlendY",
            "kDampedComDraw",
            "kDampedComErrorBlend",
            "kDampedComVelBlend",
            "kDampedComVelBlendY",
            "kDampedComErrorIterBlend",
            "kSkaterPoolUpdateInParallel",
            "kDrawDeckAngleCompression",
            "kDrawNewPump",
            "kTimeToConsiderPumpingMode",
            "kShowBodyTiltInfo",
            "kShowCoffinInfo",
            "kShowReckoningInfo",
            "kBodySpinBlend",
            "kBodySpinDerivativeScalar",
            "kDerivativeThreshold",
            "kBodySpinInputBlend",
            "kBodySpinInputFade",
            "kMaxAutoSpinMag",
            "kMaxBodySpinSpeedToForceAutoAdjust",
            "kEnableFreezing",
            "kGroundProportionalBlend",
            "kGroundMaxCosAngleDelta",
            "kMinDPWithZToBeConsideredDeckEnd",
            "kDrawAbsorptionCalc",
            "kDrawAntiFlipTorque",
            "kDrawSBJoints",
            "kDrawJump",
            "kDrawPumpForceIntention",
            "kDrawVertTurning",
            "kDrawVertTurningInput",
            "kDrawWallRide",
            "kFreeSkateboardAngularDamping",
            "kFreeSkateboardWheelInertia",
            "kSkateboard_LineDistToUseNormal",
            "kDotToStopPush",
            "kbDrawJumpDebugInfo",
            "kbDrawNudgeDebugInfo",
            "kSKC_HandDriveDisp",
            "kSKC_HandDriveStr",
            "kSKC_UseHandDrives",
            "kSKC_UseSoftHook",
            "kSkateboardLinSoftDriveDisp",
            "kSkateboardLinSoftDriveStr",
            "kSkateboardSoftDriveDisp",
            "kSkateboardSoftDriveStr",
            "kDoWipeoutCameraRootMtxBlend",
            "kCollisionPushFraction",
            "kSkeletonOB_CoreCollideWithLW",
            "kDrawContactInfo",
            "kDrawIKContactDebug",
            "kPSK_DrawHookToHips",
            "kSidewaysOffsetDraw",
            "kDrawSkateboardError",
            "kDrawSkeletonCollisionWipeout",
            "kSkeletonDisplayTrajectoryNode",
            "kEnableIKContactOffset",
            "kFollowAnimInRagdoll",
            "kLandingAdjustFramesToBlendAwayOffset",
            "kSkeletonOB_HaveBodyCollision",
            "kHeightOffsetY",
            "kLandingAdjustFirstFrameHack",
            "kMaxSkeletonDisp",
            "kMaxIKContactOffset",
            "kMaxIKContactPenetrationOffset",
            "kMaxProxyYDist",
            "kPS_ScaleHookToHipsY",
            "kDebugLandingAdjust",
            "kSidewaysOffset",
            "kSoftDamping",
            "kUpdatePoseBuffer",
            "kPS_UseNormalsForProxyOffset",
            "kSkeletonOB_UseLightDMOGroup",
            "kSB_UseRagdollJointLimits",
            "kDebugUsephyscogland",
            "kZeroTrajectoryTransform",
            "kSCDistToBeOnSamePlane",
            "kSCDotValueToBeOpposing",
            "kAllowAnyContact",
            "kDebugAlwayCollide",
            "kDrawAudioCollisionInfo",
            "kDrawBodyPartCollision",
            "kDrawCollisionUpdate",
            "kShowGroundPlane",
            "kSCMaxDistOffBoard",
            "kSmallObjectForceScalarHack",
            "kHeadTrackingClampIdeal",
            "kDebugForceLookAt",
            "kDrawHeadAdjustInfo",
            "kFractionalHeadAdjust",
            "kFractionalSpineAdjust",
            "kHeadTrackingLargestUpAngle",
            "kHeadTrackingMaxAngleToLookAtTargetXYZ",
            "kDOPrePhysIK",
            "kMaxLengthTestArm",
            "kMaxLengthTestLeg",
            "kDoNewPostPhysicsIK",
            "kDrawFeetOnDeckIK",
            "kDrawNewPostPhysicsIK",
            "kDrawPostFeetIK",
            "kDrawPostPhysIK",
            "kDrawPreFeetIK",
            "kEXIK_MaxAngle",
            "kMaxIKLocalDelta",
            "kMaxIKOffsetLength",
            "kMaxNormalAngleDelta",
            "kToeContactThreshold",
            "kTargetNormalBlendSpeed",
            "kSpeedConservationDebugDisplay",
            "kbLandingOnDeck_DebugDraw",
            "kbLOD_State_DebugDraw",
            "kbLandingOnDeck_DebugDrawTraj",
            "kbLOD_EnableFeetIK",
            "kLOD_EnableSpin",
            "kMaxLandOnBoardAssist",
            "kMaxLandingError",
            "kMaxLandingErrorAI",
            "kLOD_MaxTimeAdjustProp",
            "kLOD_MinDotFotFeetIK",
            "kLOD_MinTimeAdjustProp",
            "kLOD_MinTimeToJumpOnDeck",
            "kbLOD_NegateY",
            "kOffboardAirMaxLandOnBoardAssist",
            "kbLOD_ShowLandingAdjust",
            "kStandingJumpMaxLandOnBoardAssist",
            "kLOD_TimeForLandingWipeOut",
            "kLOD_TimeToEnableIk",
            "kLOD_TrajError",
            "kLOD_AngleTransitionTime",
            "kUseAccurateIKError",
            "kLOD_ZeroY",
            "kbLOD_DeckYOffset",
            "kLOD_TrajOffSetY",
            "kLOD_TrajRadius",
            "kbLOD_WipeoutTime",
            "kOB_DebugCollResponseA",
            "kDoCOMVelBlendOut",
            "kFramesToBlendOutStartingComVel",
            "kOB_GroundMaxDelta",
            "kOB_GroundPropBlend",
            "kOB_MaxSkeletonDisp",
            "kOB_MaxLandingSpeedForBlend",
            "kOB_MinTrajW",
            "kOB_MinUpVectorY",
            "kDrawOffBoardAirSkeleton",
            "kEnableOffBoardWipeouts",
            "kEnableRepushingTimeLimit",
            "kRunOutMinAngleForWipeoutAir",
            "kTrajectoryConeAngleScalar",
            "kbOB_AIR_DebugDraw",
            "kOB_Debug_Traj_Fail",
            "kJumpForwardDistance",
            "kRunOutMaxAngleForLocomotion",
            "kOBAir_MinUpVectorY",
            "kMinTimeInStateForSkateboardWipeout",
            "kMinTimeInAirForRunoff",
            "kRunOutMinAngleForWipeout",
            "kOBA_NumTrajForMount",
            "kRunoutProportionalAngle",
            "kRunoutTransitionTime",
            "kRunoutUpwardVelocityNudge",
            "kRunoutZXVelocityReduction",
            "kRunoutTimeToLandScalar",
            "kOB_AIR_TrajectoryMaxErrorEnd",
            "kOB_AIR_TrajectoryMaxErrorStart",
            "kOB_AIR_TrajectoryMaxTime",
            "kOffBoarAirYSpeedForWipeout",
            "kOBA_BoardPosDelta",
            "kOBA_TrajPosDelta",
            "kPhysicsAirMountLaunchTrajMaxChange",
            "kPhysicsAirJumpFixFrames",
            "kPhysicsAirJumpFixMaxError",
            "kJumpVelMinYError",
            "kbDrawPhysicsAirDebugInfo",
            "kAntiFlipNudge",
            "kAntiFlipNudgeMaxDeckSpeed",
            "kHangFrames",
            "kHungUpWipeoutFrames",
            "kMaxGroundPinFrames",
            "kMaxGroundPinStartFrames",
            "kMinAntiFlipFrames",
            "kTimeToAvoidVehicleAfterSkitching",
            "kFallThroughWorldResetFrames",
            "kGrindJumpAirFrames",
            "kMaxGrindIllegalFrames",
            "kStateChangerMinTimeInAir",
            "kDisableSkitching",
            "kForcedWorldSurface",
            "kOBLT_LineLength",
            "kOBLT_StartHeight",
            "kOBLT_TestRadius",
            "kOBLT_MinNormalDotTurn",
            "kOBLT_MinYNormalTest",
            "kOBLT_NumLines",
            "kOBLT_TrajEndHeight",
            "kOBLT_TrajEndLength",
            "kOBLT_TrajStartHeight",
            "kOBLT_TurnSpeed",
            "kEnableOffBoardFeetIK",
            "kOBIK_TimeToBlendOutFixedFootPos",
            "kYMinHeightToBeOffGround",
            "kOBIK_FramesToWipeoutForBadGround",
            "kOBIK_FramesToWipeoutForFeetNotDown",
            "kHeightAboveGroundToWipeout",
            "kHeightAboveGroundFootTest",
            "kOBIK_IKBlendVal",
            "kOBIK_IKMaxYDistUp",
            "kOBIK_IKMaxYFootNormal",
            "kOBIK_IKMaxYLiftUp",
            "kFeetLineTestRadius",
            "kOBIK_FramesForGroundCorrection",
            "kOB_IK_MaxFeetYDist",
            "kOBIK_MinTrajValue",
            "kSlopeEffectScalar",
            "kOBIK_SpeedAwayFromBadFoot",
            "kYNormalValueToWipeout",
            "kYHeightToBeOffGround",
            "kDebugDrawWobble",
            "kFramesForTeleportCountdown",
            "kbDebugDraw",
            "kSelectCentralTrajectory",
            "kShowRawTrajectory",
            "kShowTrajectories",
            "kShowTrajectorySelect",
            "kShowWallInfo",
            "kWipeAutoYAxisScalar",
            "kWipeAutoZAxisScalar",
            "kWipeoutDebugDisplay",
            "kWO_DragStart",
            "kUpVelocityScaleLower",
            "kMaxAISkaterBailDurationInSeconds",
            "kWO_MaxDrag",
            "kMaxSkeletonPartVelocity",
            "kMinTimeToBeImpaledForTeleport",
            "kWipeControlMaxTorque",
            "kMaxWipeoutImpulseFrames",
            "kWipeControlMaxAngleRotX",
            "kWipeControlMaxAngleRotY",
            "kMinControlToStopAutoTwist",
            "kMinUserWipeoutRecoverSec",
            "kWipeout_SlowSpeed",
            "kWO_TimeToFadeInDrag",
            "kWipeoutTorqueForce",
            "kUpVelocityScaleUpper",
            "kWipeoutAirToHardGrindEnabled",
            "kWipeoutFailureToInvExitEnabled",
            "kWipeoutBadAngleLandingEnabled",
            "kWipeoutBadSkateboardEnabled",
            "kWipeoutOB_BadGroundEnabled",
            "kWipeoutBoardslideStopOnRailEnabled",
            "kWipeoutOB_DeathLeapEnabled",
            "kWipeoutDeckFlippedEnabled",
            "kWipeoutDeckOpposingContactsEnabled",
            "kWipeoutHardGrindEngagementEnabled",
            "kWipeoutHardHorizontalGrindEnabled",
            "kWipeoutVehicleCollisionEnabled",
            "kWipeoutHungUpOnRailEnabled",
            "kWipeoutHeavyLandingEnabled",
            "kWipeoutOB_LandedOnDeckEnabled",
            "kWipeoutOB_LandingTooFastEnabled",
            "kWipeoutOB_NotUprightEnabled",
            "kWipeoutOffBalanceEnabled",
            "kWipeoutOffBalanceGrindEnabled",
            "kWipeoutFailureToGoOffboardEnabled",
            "kWipeoutOffboardEnabled",
            "kWipeoutDeckAccelerationEnabled",
            "kWipeoutSensitiveSkeletonCollEnabled",
            "kWipeoutSkeletonContactEnabled",
            "kWipeoutSkeletonDisplacementEnabled",
            "kWipeoutLeaningContactEnabled",
            "kWipeoutOpposingContactsEnabled",
            "kWipeoutSkeletonSquashedEnabled",
            "kWipeoutTooFastOffBoardEnabled",
            "kWipeoutUpsideDownFallingEnabled",
            "kWipeoutBodyFlipEnabled",
            "kWipeoutDisplayDebugInfo",
            "kWipeoutDisplayOffBoardInfo",
            "kNumBatchTests",
            "kNumBoxBatches",
            "kNumLineBatches",
            "kNumSkaterTrajectoryBatches",
            "cv_EnableRopa",
            "kNearClipPlaneDistance",
            "kSkaterFadeInTime",
            "kSkaterFadeOutTime",
            "kbUseGeneratedStateGraph",
            "kbTestDeterminism",
            "kEnableTextureStream",
        ],
    );
    assert!(
        recorded_is_subsequence,
        "the recorded title-order list is no longer an ordered subsequence of the linked labels"
    );
    manager.try_deserialize_startup(None).unwrap();
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkUnrelated=9\r\n"))
        .unwrap();
    let mut provider = refs.fill_provider();
    provider.capture_float(NewPushAllowance).unwrap();
    assert_eq!(
        provider.float(NewPushAllowance).unwrap().to_bits(),
        0x3e99_999a
    );
}

#[test]
fn hand_drives_adapter_uses_actual_linked_target_and_retains_capture_across_rebind() {
    use crate::simulation_assembly_backend::constraint_session::physical_skateboard_controller::SkateboardControllerCvars;
    let mut refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.capture_use_hand_drives(),
        Err(InputCvarError::Unlinked("kSKC_UseHandDrives"))
    ));
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    let target = refs.capture_use_hand_drives().unwrap();
    assert!(refs.use_hand_drives(&target).unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkSKC_UseHandDrives=0\r\n"))
        .unwrap();
    assert!(!refs.use_hand_drives(&target).unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    let current = refs.capture_use_hand_drives().unwrap();
    assert!(refs.use_hand_drives(&current).unwrap());
    assert!(!refs.use_hand_drives(&target).unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkSKC_UseHandDrives=-1\r\n"))
        .unwrap();
    assert!(refs.use_hand_drives(&target).unwrap());
}

#[test]
fn vector_ini_preserves_current_on_incomplete_tokens_and_preserves_hidden_lanes() {
    let refs = InputCvarReferences::new_static_initialized();
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    let target = refs.targets.lock().unwrap()[11].as_ref().unwrap().clone();
    *target.lock().unwrap() = Value::Vector2([0.25, 0.5, 7.0, -0.0]);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkInputHistoryDeadZone=9,,,,\r\n"))
        .unwrap();
    let Value::Vector2(value) = *target.lock().unwrap() else {
        panic!("vector target");
    };
    assert_eq!(
        value.map(f32::to_bits),
        [
            0.25f32.to_bits(),
            0.5f32.to_bits(),
            7f32.to_bits(),
            (-0f32).to_bits()
        ]
    );
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkInputHistoryDeadZone=,,word,,2,9\r\n"))
        .unwrap();
    let Value::Vector2(value) = *target.lock().unwrap() else {
        panic!("vector target");
    };
    assert_eq!(
        value.map(f32::to_bits),
        [
            2f32.to_bits(),
            2f32.to_bits(),
            7f32.to_bits(),
            (-0f32).to_bits()
        ]
    );
}
#[test]
fn vbr_consumer_rereads_live_reference_each_time() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(refs.try_use_vbr(), Err(InputCvarError::Unlinked("kUseVBR")));
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(refs.try_use_vbr().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkUseVBR=0\r\n"))
        .unwrap();
    assert!(!refs.try_use_vbr().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(refs.try_use_vbr().unwrap());
}

#[test]
fn integer_deserialize_preserves_metadata_without_clamp_and_rebinds_actual_target() {
    use crate::simulation_assembly_backend::constraint_session::system_logic::StateChangerCvars;
    let mut refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.fall_through_world_reset_frames(),
        Err(InputCvarError::Unlinked("kFallThroughWorldResetFrames"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.fall_through_world_reset_frames().unwrap(), 400);
    let target = Arc::clone(refs.targets.lock().unwrap()[19].as_ref().unwrap());
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkFallThroughWorldResetFrames=prefix-7tail\r\n",
        ))
        .unwrap();
    assert_eq!(refs.fall_through_world_reset_frames().unwrap(), -7);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkFallThroughWorldResetFrames=5000\r\n"))
        .unwrap();
    assert_eq!(refs.fall_through_world_reset_frames().unwrap(), 5000);
    let Value::Integer {
        value,
        minimum,
        maximum,
    } = *target.lock().unwrap()
    else {
        panic!("integer");
    };
    assert_eq!((value, minimum, maximum), (5000, 0, 1000));
    let label = manager
        .labels
        .iter()
        .find(|label| label.definition.label == "kFallThroughWorldResetFrames")
        .unwrap();
    let Value::Integer { value: default, .. } = label.definition.initial else {
        panic!("integer default");
    };
    assert_eq!(default, 400);
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkFallThroughWorldResetFrames=none\r\n"))
        .unwrap();
    assert_eq!(refs.fall_through_world_reset_frames().unwrap(), 0);
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert_eq!(refs.fall_through_world_reset_frames().unwrap(), 400);
}

#[test]
fn graph_boot_flags_follow_fresh_targets_and_live_startup_values() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_test_determinism(),
        Err(InputCvarError::Unlinked("kbTestDeterminism"))
    );
    assert_eq!(
        refs.try_use_generated_state_graph(),
        Err(InputCvarError::Unlinked("kbUseGeneratedStateGraph"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_test_determinism().unwrap());
    assert!(refs.try_use_generated_state_graph().unwrap());
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkbTestDeterminism=1\r\nkbUseGeneratedStateGraph=0\r\n",
        ))
        .unwrap();
    assert!(refs.try_test_determinism().unwrap());
    assert!(!refs.try_use_generated_state_graph().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(!refs.try_test_determinism().unwrap());
    assert!(refs.try_use_generated_state_graph().unwrap());
}

#[test]
fn adjust_spawn_position_reads_linked_current_boolean_after_override_and_relink() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_enable_adjust_spawn_position(),
        Err(InputCvarError::Unlinked("kbEnableAdjustSpawnPosition"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(refs.try_enable_adjust_spawn_position().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkbEnableAdjustSpawnPosition=0\r\n"))
        .unwrap();
    assert!(!refs.try_enable_adjust_spawn_position().unwrap());

    // Reusing one manager preserves its live target; another manager binds a fresh default.
    first.try_link(&refs).unwrap();
    assert!(!refs.try_enable_adjust_spawn_position().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(refs.try_enable_adjust_spawn_position().unwrap());
    second
        .try_deserialize_startup(Some(b"[Config]\r\nkbEnableAdjustSpawnPosition=false\r\n"))
        .unwrap();
    assert!(!refs.try_enable_adjust_spawn_position().unwrap());
}

#[test]
fn skitch_disable_reads_actual_linked_boolean_each_time() {
    use crate::simulation_assembly_backend::constraint_session::system_logic::StateChangerCvars;
    let mut refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.disable_skitching(),
        Err(InputCvarError::Unlinked("kDisableSkitching"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.disable_skitching().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkDisableSkitching=1\r\n"))
        .unwrap();
    assert!(refs.disable_skitching().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(!refs.disable_skitching().unwrap());
}

#[test]
fn animation_jobs_read_follows_target_changes_and_reference_rebinding() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_use_jobs_for_skater_anim_update(),
        Err(InputCvarError::Unlinked("kUseJobsForSkaterAnimUpdate"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(refs.try_use_jobs_for_skater_anim_update().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkUseJobsForSkaterAnimUpdate=0\r\n"))
        .unwrap();
    assert!(!refs.try_use_jobs_for_skater_anim_update().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(refs.try_use_jobs_for_skater_anim_update().unwrap());
}

#[test]
fn show_velocity_removal_is_false_only_until_actual_target_changes() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_show_vel_removal(),
        Err(InputCvarError::Unlinked("kShowVelRemoval"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(!refs.try_show_vel_removal().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkShowVelRemoval=1\r\n"))
        .unwrap();
    assert!(refs.try_show_vel_removal().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(!refs.try_show_vel_removal().unwrap());
}

#[test]
fn gesture_enum_metadata_raw_ini_values_and_live_relinking() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_debug_gesture_up(),
        Err(InputCvarError::Unlinked("kAnimUpGesture"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_enable_debug_gestures().unwrap());
    assert_eq!(
        [
            refs.try_debug_gesture_up().unwrap(),
            refs.try_debug_gesture_down().unwrap(),
            refs.try_debug_gesture_left().unwrap(),
            refs.try_debug_gesture_right().unwrap()
        ],
        [34, 33, 32, 31]
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkEnableDebugGestures=1\r\nkAnimUpGesture=999\r\nkAnimDownGesture=-7\r\nkAnimLeftGesture=Fonz\r\n")).unwrap();
    assert!(refs.try_enable_debug_gestures().unwrap());
    assert_eq!(refs.try_debug_gesture_up().unwrap(), 999);
    assert_eq!(refs.try_debug_gesture_down().unwrap(), -7);
    assert_eq!(refs.try_debug_gesture_left().unwrap(), 0);
    let label = manager
        .labels
        .iter()
        .find(|l| l.definition.label == "kAnimUpGesture")
        .unwrap();
    let Value::Enumeration { value, tags } = *label.target.lock().unwrap() else {
        panic!("enum kind lost")
    };
    assert_eq!(value, 999);
    assert_eq!(tags.len(), 35);
    assert_eq!(tags[5], ("Fonz", 5));
    assert_eq!(tags[34], ("Axe", 34));
    let Value::Enumeration { value: default, .. } = label.definition.initial else {
        panic!("enum default kind lost")
    };
    assert_eq!(default, 34);
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_debug_gesture_up().unwrap(), 999);
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert_eq!(refs.try_debug_gesture_up().unwrap(), 34);
    assert!(!refs.try_enable_debug_gestures().unwrap());
}

#[test]
fn repushing_time_limit_reads_live_target_after_relink() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_enable_repushing_time_limit(),
        Err(InputCvarError::Unlinked("kEnableRepushingTimeLimit"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(refs.try_enable_repushing_time_limit().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkEnableRepushingTimeLimit=0\r\n"))
        .unwrap();
    assert!(!refs.try_enable_repushing_time_limit().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(refs.try_enable_repushing_time_limit().unwrap());
}

#[test]
fn bump_enable_reads_live_target_and_native_true_default() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(refs.try_motion_use_bump().is_err());
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_motion_use_bump().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkUseBump=0\r\n"))
        .unwrap();
    assert!(!refs.try_motion_use_bump().unwrap());
}

#[test]
fn mounted_recipe_binary_choice_reads_live_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_schema_load_from_binary(),
        Err(InputCvarError::Unlinked("kSchemaLoadFromBinary"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_schema_load_from_binary().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkSchemaLoadFromBinary=false\r\n"))
        .unwrap();
    assert!(!refs.try_schema_load_from_binary().unwrap());
}

#[test]
fn juice_retry_cvar_keeps_default_false_and_reads_live_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_juice_connection_retry(),
        Err(InputCvarError::Unlinked("kEnableJuiceConnectionRetry"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(!refs.try_juice_connection_retry().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\nkEnableJuiceConnectionRetry=1\r\n"))
        .unwrap();
    assert!(refs.try_juice_connection_retry().unwrap());
}

#[test]
fn ropa_live_target_defaults_enabled_and_reloads() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_enable_ropa(),
        Err(InputCvarError::Unlinked("cv_EnableRopa"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_enable_ropa().unwrap());
    manager
        .try_deserialize_startup(Some(b"[Config]\r\ncv_EnableRopa=0\r\n"))
        .unwrap();
    assert!(!refs.try_enable_ropa().unwrap());
}

#[test]
fn n_dimensional_blend_space_slicing_has_its_own_live_target() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_blend_space_slicing_n(),
        Err(InputCvarError::Unlinked("kUseSlicingProjectionN"))
    );
    let mut first = InputCvarManager::new();
    first.try_link(&refs).unwrap();
    assert!(refs.try_blend_space_slicing_n().unwrap());
    assert!(refs.try_blend_space_slicing3().unwrap());
    first
        .try_deserialize_startup(Some(b"[Config]\r\nkUseSlicingProjectionN=0\r\n"))
        .unwrap();
    assert!(!refs.try_blend_space_slicing_n().unwrap());
    assert!(refs.try_blend_space_slicing3().unwrap());
    let mut second = InputCvarManager::new();
    second.try_link(&refs).unwrap();
    assert!(refs.try_blend_space_slicing_n().unwrap());
    assert!(refs.try_blend_space_slicing3().unwrap());
}

#[test]
fn walking_targets_are_live_and_unlinked_reads_do_not_use_defaults() {
    let refs = InputCvarReferences::new_static_initialized();
    assert_eq!(
        refs.try_step_shoe_offset(),
        Err(InputCvarError::Unlinked("kShoeOffset"))
    );
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.try_step_shoe_offset().unwrap().to_bits(), 0x3c75c28f);
    assert_eq!(refs.try_step_sprint_trajectory_count().unwrap(), 3);
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkShoeOffset=0.025\r\nkNbTrajWhenSprinting=4\r\n",
        ))
        .unwrap();
    assert_eq!(refs.try_step_shoe_offset().unwrap(), 0.025);
    assert_eq!(refs.try_step_sprint_trajectory_count().unwrap(), 4);
}

#[test]
fn locomotion_collision_turn_cvars_are_live() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_current_float(470),
        Err(InputCvarError::Unlinked("kAdditionalAngleAway"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    for (i, value) in [(470, 0.0), (471, 2.5), (472, 60.0), (473, 15.0)] {
        assert_eq!(refs.try_current_float(i).unwrap(), value);
    }
    manager.try_deserialize_startup(Some(b"[Config]\r\nkAdditionalAngleAway=12\r\nkOBCOLL_MaxTurnAngleDegs=3\r\nkOBCOLL_MaxAutoStartTurnAngle=40\r\nkOBCOLL_MaxAutoStartTurnAngleAir=20\r\n")).unwrap();
    for (i, value) in [(470, 12.0), (471, 3.0), (472, 40.0), (473, 20.0)] {
        assert_eq!(refs.try_current_float(i).unwrap(), value);
    }
}

#[test]
fn create_locomotion_live_cvars_preserve_defaults_and_reload() {
    let refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.try_create_locomotion_start_y_offset(),
        Err(InputCvarError::Unlinked("kLocomotionStartYOffset"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert!(refs.try_create_locomotion_sprint_button().unwrap());
    assert_eq!(refs.try_create_locomotion_start_y_offset().unwrap(), 0.5);
    assert_eq!(refs.try_create_locomotion_failure_frame_count().unwrap(), 3);
    assert_eq!(
        refs.try_create_locomotion_maximum_raw_time()
            .unwrap()
            .to_bits(),
        0x3ca3d70a
    );
    manager.try_deserialize_startup(Some(b"[Config]\r\nkUseSprintButton=0\r\nkLocomotionStartYOffset=0.75\r\nkMaxFramesForStepSelectorToFail=999\r\n")).unwrap();
    assert!(!refs.try_create_locomotion_sprint_button().unwrap());
    assert_eq!(refs.try_create_locomotion_start_y_offset().unwrap(), 0.75);
    assert_eq!(
        refs.try_create_locomotion_failure_frame_count().unwrap(),
        999
    );
}

#[test]
fn offboard_location_live_cvars_preserve_source_defaults_and_unclamped_reload() {
    use crate::physical_player_remaining_constructor_owners::location_runtime::LocationTuning;
    let mut refs = InputCvarReferences::new_static_initialized();
    assert!(matches!(
        refs.line_count(),
        Err(InputCvarError::Unlinked("kOBLT_NumLines"))
    ));
    let mut manager = InputCvarManager::new();
    manager.try_link(&refs).unwrap();
    assert_eq!(refs.line_count().unwrap(), 3);
    assert_eq!(refs.test_radius().unwrap().to_bits(), 0x3e28f5c3);
    assert_eq!(refs.minimum_normal_dot().unwrap().to_bits(), 0xbf666666);
    assert_eq!(refs.turn_speed().unwrap(), 2.0);
    assert_eq!(refs.trajectory_start_height().unwrap(), 1.5);
    manager
        .try_deserialize_startup(Some(
            b"[Config]\r\nkOBLT_NumLines=999\r\nkOBLT_TurnSpeed=1.25\r\n",
        ))
        .unwrap();
    assert_eq!(refs.line_count().unwrap(), 999);
    assert_eq!(refs.turn_speed().unwrap(), 1.25);
}
