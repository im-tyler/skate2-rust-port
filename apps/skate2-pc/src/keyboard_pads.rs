//! The keyboard as pad slot zero when no SDL gamepad is connected (PC port
//! addition). The layout is the Skate 3 browser port's published keyboard
//! scheme, adopted behaviorally: every mapping is digital — a key is either
//! fully deflected or neutral, with no ramps — exactly as that port's loader
//! reports it (`Space` A, `E` B, `Q` X, `R` Y, `Z`/`X` shoulders, `Escape`
//! Start, `Tab` Back, `V`/`B` stick presses, `IJKL` the d-pad, `WASD` the left
//! stick, the arrows the right stick, `Shift` the left trigger, `C` the right
//! trigger). No code from that GPL project is included; the integer semantics
//! are the console `XInput` ones [`XboxPadSample`] keeps.
//!
//! Only the SDL platform (`crate::sdl_pad_platform`) mounts it; the Windows
//! `XInput` platform keeps its slots as the console reported them.

use skate2_core::XboxPadSample;
use winit::{
    event::ElementState,
    keyboard::{KeyCode, PhysicalKey},
};

/// One button key: the `XInput` bit it holds down.
type ButtonKey = (KeyCode, u16);

/// The button keys, in the Skate 3 browser port's order.
const BUTTON_KEYS: [ButtonKey; 14] = [
    (KeyCode::Space, 0x1000),  // A
    (KeyCode::KeyE, 0x2000),   // B
    (KeyCode::KeyQ, 0x4000),   // X
    (KeyCode::KeyR, 0x8000),   // Y
    (KeyCode::KeyZ, 0x0100),   // Left shoulder
    (KeyCode::KeyX, 0x0200),   // Right shoulder
    (KeyCode::Escape, 0x0010), // Start
    (KeyCode::Tab, 0x0020),    // Back
    (KeyCode::KeyV, 0x0040),   // Left stick press
    (KeyCode::KeyB, 0x0080),   // Right stick press
    (KeyCode::KeyI, 0x0001),   // D-Up
    (KeyCode::KeyK, 0x0002),   // D-Down
    (KeyCode::KeyJ, 0x0004),   // D-Left
    (KeyCode::KeyL, 0x0008),   // D-Right
];

/// One stick axis key: the channel and the sign it pushes, at full digital
/// deflection. Channels: left x/y then right x/y, `Y` up.
type AxisKey = (KeyCode, usize);

/// The stick keys: `WASD` steers, the arrows flick.
const AXIS_KEYS: [AxisKey; 8] = [
    (KeyCode::KeyA, 0),
    (KeyCode::KeyD, 0),
    (KeyCode::KeyS, 1),
    (KeyCode::KeyW, 1),
    (KeyCode::ArrowLeft, 2),
    (KeyCode::ArrowRight, 2),
    (KeyCode::ArrowDown, 3),
    (KeyCode::ArrowUp, 3),
];

/// Full deflection, the only magnitude the digital scheme knows.
const FULL: i16 = 32_767;

/// The keys whose state the pad tracks, buttons first, then axes, then the
/// triggers.
const MAPPED: usize = BUTTON_KEYS.len() + AXIS_KEYS.len() + 2;

/// The keyboard read as a digital pad, with the packet number `XInput` would
/// have reported: it moves only when the sample does.
#[derive(Debug, Default)]
pub(crate) struct KeyboardPad {
    /// One held flag per mapped key.
    held: [bool; MAPPED],
    /// The packet number of the last reported sample.
    packet: u32,
    /// The last reported sample; packets advance only on change.
    last: Option<XboxPadSample>,
}

impl KeyboardPad {
    /// The index of a key's held flag, or `None` when the key is unmapped.
    fn slot_of(code: KeyCode) -> Option<usize> {
        BUTTON_KEYS
            .iter()
            .position(|&(key, _)| key == code)
            .or_else(|| {
                AXIS_KEYS
                    .iter()
                    .position(|&(key, _)| key == code)
                    .map(|index| BUTTON_KEYS.len() + index)
            })
            .or_else(|| {
                (code == KeyCode::ShiftLeft || code == KeyCode::ShiftRight).then_some(MAPPED - 2)
            })
            .or_else(|| (code == KeyCode::KeyC).then_some(MAPPED - 1))
    }

    /// One physical key event; unmapped keys pass through unheard.
    pub(crate) fn key_event(&mut self, key: PhysicalKey, state: ElementState) {
        if let PhysicalKey::Code(code) = key
            && let Some(slot) = Self::slot_of(code)
        {
            self.held[slot] = state == ElementState::Pressed;
        }
    }

    /// Releases everything, as the browser port does when its window loses
    /// focus or a text box takes the keyboard.
    pub(crate) fn clear(&mut self) {
        self.held = [false; MAPPED];
    }

    /// The slot's current sample and packet, as `XInputGetState` would report
    /// them: the packet advances only while the sample changes.
    pub(crate) fn claim(&mut self) -> (u32, XboxPadSample) {
        let mut buttons = 0_u16;
        for (index, (_, bit)) in BUTTON_KEYS.iter().enumerate() {
            if self.held[index] {
                buttons |= bit;
            }
        }
        let axis = |channel: usize| {
            let mut value = 0_i16;
            for (index, (_, axis_channel)) in AXIS_KEYS.iter().enumerate() {
                if *axis_channel == channel {
                    let sign: i16 = if index % 2 == 0 { -FULL } else { FULL };
                    if self.held[BUTTON_KEYS.len() + index] {
                        value += sign;
                    }
                }
            }
            value
        };
        let trigger = |slot: usize| if self.held[slot] { 255 } else { 0 };
        let sample = XboxPadSample {
            buttons,
            left_trigger: trigger(MAPPED - 2),
            right_trigger: trigger(MAPPED - 1),
            left_x: axis(0),
            left_y: axis(1),
            right_x: axis(2),
            right_y: axis(3),
        };
        if self.last != Some(sample) {
            self.packet = self.packet.wrapping_add(1);
            self.last = Some(sample);
        }
        (self.packet, sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral() -> XboxPadSample {
        XboxPadSample {
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        }
    }

    #[test]
    fn idle_keyboard_is_a_neutral_pad() {
        let mut pad = KeyboardPad::default();
        let (packet, sample) = pad.claim();
        assert_eq!(sample, neutral());
        assert_eq!(packet, 1, "the first report moves the packet");
        assert_eq!(pad.claim().0, 1, "an unchanged sample keeps the packet");
    }

    #[test]
    fn buttons_carry_the_skate3_scheme_bits() {
        let mut pad = KeyboardPad::default();
        for (key, _) in BUTTON_KEYS {
            pad.key_event(PhysicalKey::Code(key), ElementState::Pressed);
        }
        let (_, sample) = pad.claim();
        assert_eq!(sample.buttons, 0xF3FF);
        pad.key_event(PhysicalKey::Code(KeyCode::KeyE), ElementState::Released);
        let (_, sample) = pad.claim();
        assert_eq!(
            sample.buttons,
            0xF3FF & !0x2000,
            "releasing B clears only B"
        );
    }

    #[test]
    fn sticks_are_digital_in_both_directions() {
        let mut pad = KeyboardPad::default();
        pad.key_event(PhysicalKey::Code(KeyCode::KeyD), ElementState::Pressed);
        pad.key_event(PhysicalKey::Code(KeyCode::KeyW), ElementState::Pressed);
        pad.key_event(PhysicalKey::Code(KeyCode::ArrowLeft), ElementState::Pressed);
        pad.key_event(PhysicalKey::Code(KeyCode::ArrowDown), ElementState::Pressed);
        let (_, sample) = pad.claim();
        assert_eq!((sample.left_x, sample.left_y), (FULL, FULL));
        assert_eq!((sample.right_x, sample.right_y), (-FULL, -FULL));
        // Opposite keys hold the centre, as two digital directions cancel.
        pad.key_event(PhysicalKey::Code(KeyCode::KeyA), ElementState::Pressed);
        let (_, sample) = pad.claim();
        assert_eq!(sample.left_x, 0);
    }

    #[test]
    fn shift_is_the_left_trigger_and_c_the_right() {
        let mut pad = KeyboardPad::default();
        pad.key_event(
            PhysicalKey::Code(KeyCode::ShiftRight),
            ElementState::Pressed,
        );
        pad.key_event(PhysicalKey::Code(KeyCode::KeyC), ElementState::Pressed);
        let (_, sample) = pad.claim();
        assert_eq!((sample.left_trigger, sample.right_trigger), (255, 255));
        assert_eq!(sample.buttons, 0, "the triggers carry no button bits");
    }

    #[test]
    fn unmapped_keys_are_ignored_and_clear_releases_everything() {
        let mut pad = KeyboardPad::default();
        pad.key_event(PhysicalKey::Code(KeyCode::KeyT), ElementState::Pressed);
        pad.key_event(PhysicalKey::Code(KeyCode::Space), ElementState::Pressed);
        let (_, sample) = pad.claim();
        assert_eq!(sample.buttons, 0x1000);
        pad.clear();
        assert_eq!(pad.claim().1, neutral());
    }

    #[test]
    fn packets_advance_only_while_the_sample_changes() {
        let mut pad = KeyboardPad::default();
        assert_eq!(pad.claim().0, 1);
        assert_eq!(pad.claim().0, 1);
        pad.key_event(PhysicalKey::Code(KeyCode::Space), ElementState::Pressed);
        assert_eq!(pad.claim().0, 2);
        assert_eq!(pad.claim().0, 2);
        pad.key_event(PhysicalKey::Code(KeyCode::Space), ElementState::Released);
        assert_eq!(pad.claim().0, 3);
    }
}
