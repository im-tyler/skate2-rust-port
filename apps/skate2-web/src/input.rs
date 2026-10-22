//! Browser pads for the WebAssembly build: the keyboard and the Gamepad API as
//! [`XboxPadSample`]s, exactly as the native builds' `XInput` and SDL platforms
//! produce them (PC/browser port addition; the reference console read its pads
//! through `XInput`, whose integer semantics [`XboxPadSample`] keeps).
//!
//! The `XInput` button bits are the ones the native SDL platform folds into a
//! sample (`apps/skate2-pc/src/sdl_pads.rs`), and the browser button order is
//! the W3C standard gamepad mapping (`Navigator.getGamepads()`, buttons 0–15:
//! A, B, X, Y, LB, RB, LT, RT, Back, Start, L3, R3, D-Up, D-Down, D-Left,
//! D-Right; axes 0–3: left X, left Y, right X, right Y, Y pointing down).

use skate2_core::{
    PadCapabilitiesResult, PadPlatform, PadStateResult, PadVibration, XboxPadSample,
};
use winit::{
    event::ElementState,
    keyboard::{KeyCode, PhysicalKey},
};

/// `ERROR_DEVICE_NOT_CONNECTED`, as the SDL platform reports an empty slot.
const DEVICE_NOT_CONNECTED: u32 = 1167;
/// `XINPUT_FLAG_GAMEPAD`, the only capabilities flags the game asks for.
const FLAG_GAMEPAD: u32 = 1;
/// The fixed packet the keyboard slot reports, as the native demo-sample path
/// does for a synthetic controller zero.
const KEYBOARD_PACKET: u32 = 7;

/// The `XInput` button bits, in Gamepad API standard-mapping order.
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(test)),
    expect(dead_code, reason = "the browser's gamepads only exist on WebAssembly")
)]
const BUTTON_BITS: [u16; 16] = [
    0x1000, // A (South)
    0x2000, // B (East)
    0x4000, // X (West)
    0x8000, // Y (North)
    0x0100, // Left shoulder
    0x0200, // Right shoulder
    0,      // Left trigger button: the analog value below carries it.
    0,      // Right trigger button: the analog value below carries it.
    0x0020, // Back
    0x0010, // Start
    0x0040, // Left stick press
    0x0080, // Right stick press
    0x0001, // D-Up
    0x0002, // D-Down
    0x0004, // D-Left
    0x0008, // D-Right
];

/// Analog channel indices: left stick x/y, right stick x/y, then the triggers.
const LX: usize = 0;
const LY: usize = 1;
const RX: usize = 2;
const RY: usize = 3;
const LT: usize = 4;
const RT: usize = 5;

/// One mapped key: the analog channel it pushes and the direction, or a
/// button bit. The keyboard layout (documented in `docs/WEB_PORT.md`): WASD
/// steers, the arrows flick, Q/E are the triggers, Z/C the shoulders, Space is
/// A, Shift B, Enter Start, Tab Back, 1/2 the stick presses.
type KeyMapping = (KeyCode, Option<(usize, f32)>, u16);

const MAPPED_KEYS: [KeyMapping; 18] = [
    (KeyCode::KeyW, Some((LY, 1.0)), 0),
    (KeyCode::KeyS, Some((LY, -1.0)), 0),
    (KeyCode::KeyA, Some((LX, -1.0)), 0),
    (KeyCode::KeyD, Some((LX, 1.0)), 0),
    (KeyCode::ArrowUp, Some((RY, 1.0)), 0),
    (KeyCode::ArrowDown, Some((RY, -1.0)), 0),
    (KeyCode::ArrowLeft, Some((RX, -1.0)), 0),
    (KeyCode::ArrowRight, Some((RX, 1.0)), 0),
    (KeyCode::KeyQ, Some((LT, 1.0)), 0),
    (KeyCode::KeyE, Some((RT, 1.0)), 0),
    (KeyCode::Space, None, 0x1000),
    (KeyCode::ShiftLeft, None, 0x2000),
    (KeyCode::KeyZ, None, 0x0100),
    (KeyCode::KeyC, None, 0x0200),
    (KeyCode::Enter, None, 0x0010),
    (KeyCode::Tab, None, 0x0020),
    (KeyCode::Digit1, None, 0x0040),
    (KeyCode::Digit2, None, 0x0080),
];

/// Full deflection times, in seconds: steering wants a slower stick than
/// flicking does, and the triggers sit between.
const RAMP_SECONDS: [f32; 6] = [0.15, 0.15, 0.06, 0.06, 0.12, 0.12];

/// The keyboard read as a pad, with ramped analog channels so the sticks and
/// triggers move like a pad's would instead of snapping.
#[derive(Debug, Default)]
pub(crate) struct KeyboardState {
    /// One held flag per entry of `MAPPED_KEYS`.
    held: [bool; MAPPED_KEYS.len()],
    /// Current deflection of each analog channel, -1..1 (triggers 0..1).
    channels: [f32; 6],
}

impl KeyboardState {
    /// One physical key event; unmapped keys are ignored.
    pub(crate) fn key_event(&mut self, key: KeyCode, state: ElementState) {
        let Some(index) = MAPPED_KEYS.iter().position(|(mapped, _, _)| *mapped == key) else {
            return;
        };
        self.held[index] = state == ElementState::Pressed;
    }

    /// The keyboard sample after `dt` seconds of ramping since the last one.
    pub(crate) fn sample(&mut self, dt: f32) -> XboxPadSample {
        let dt = dt.clamp(0.0, 0.1);
        for (channel, deflection) in self.channels.iter_mut().enumerate() {
            let mut target = 0.0_f32;
            for (index, (_, analog, _)) in MAPPED_KEYS.iter().enumerate() {
                if self.held[index]
                    && let Some((analog_channel, direction)) = *analog
                    && analog_channel == channel
                {
                    target += direction;
                }
            }
            if channel >= LT {
                target = target.clamp(0.0, 1.0);
            } else {
                target = target.clamp(-1.0, 1.0);
            }
            let step = (dt / RAMP_SECONDS[channel]).min(1.0);
            let difference = target - *deflection;
            *deflection += difference.clamp(-step, step);
        }
        let mut buttons = 0_u16;
        for (index, (_, _, bit)) in MAPPED_KEYS.iter().enumerate() {
            if self.held[index] {
                buttons |= bit;
            }
        }
        XboxPadSample {
            buttons,
            left_trigger: trigger_u8(f64::from(self.channels[LT])),
            right_trigger: trigger_u8(f64::from(self.channels[RT])),
            left_x: axis_i16(f64::from(self.channels[LX])),
            left_y: axis_i16(f64::from(self.channels[LY])),
            right_x: axis_i16(f64::from(self.channels[RX])),
            right_y: axis_i16(f64::from(self.channels[RY])),
        }
    }
}

/// A standard-mapping gamepad sample as the browser reports it: the sixteen
/// buttons, both analog triggers and the first four stick axes.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(test)),
    expect(dead_code, reason = "the browser's gamepads only exist on WebAssembly")
)]
pub(crate) struct StandardGamepad {
    pub(crate) buttons: [bool; 16],
    pub(crate) triggers: [f64; 2],
    pub(crate) axes: [f64; 4],
}

impl StandardGamepad {
    /// The `XInput`-shaped sample a native slot would show for this pad.
    ///
    /// The browser normalises with Y pointing down; `XInput`'s samples carry Y
    /// up, so Y is negated here. The deadzone stays where it belongs, in
    /// core's pad conversion.
    #[must_use]
    #[cfg_attr(
        all(not(target_arch = "wasm32"), not(test)),
        expect(dead_code, reason = "the browser's gamepads only exist on WebAssembly")
    )]
    pub(crate) fn sample(self) -> XboxPadSample {
        let mut buttons = 0_u16;
        for (index, pressed) in self.buttons.iter().enumerate() {
            if *pressed {
                buttons |= BUTTON_BITS[index];
            }
        }
        XboxPadSample {
            buttons,
            left_trigger: trigger_u8(self.triggers[0]),
            right_trigger: trigger_u8(self.triggers[1]),
            left_x: axis_i16(self.axes[0]),
            left_y: axis_i16(-self.axes[1]),
            right_x: axis_i16(self.axes[2]),
            right_y: axis_i16(-self.axes[3]),
        }
    }
}

/// A browser axis (-1..1, Y down) as the `XInput` i16 it would carry (Y up).
#[expect(
    clippy::cast_possible_truncation,
    reason = "the clamp bounds the value before the whole-number cast"
)]
pub(crate) fn axis_i16(value: f64) -> i16 {
    let scaled = (value.clamp(-1.0, 1.0) * 32767.0).round();
    i16::try_from(scaled as i64).expect("bounded by ±32767 before the cast")
}

/// A browser analog trigger (0..1) as `XInput`'s byte.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the clamp bounds the value before the whole-number cast"
)]
pub(crate) fn trigger_u8(value: f64) -> u8 {
    let scaled = (value.clamp(0.0, 1.0) * 255.0).round();
    u8::try_from(scaled as i64).expect("bounded by 0..=255 before the cast")
}

/// Which source a displayed sample came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PadSource {
    Keyboard,
    Gamepad { index: u32 },
}

/// The browser pad platform: a standard-mapping gamepad when one is active,
/// otherwise the keyboard, in `XInput` slot zero, exactly as the native SDL
/// platform fills the slots `XInput` leaves empty. Vibration is kept for the
/// debug HUD; the browser rumble path is future work (`docs/WEB_PORT.md`).
pub(crate) struct WebPadPlatform {
    keyboard: KeyboardState,
    /// The last standard-mapping gamepad sample and its slot.
    gamepad: Option<(u32, u32, XboxPadSample)>,
    vibration: PadVibration,
}

impl WebPadPlatform {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            keyboard: KeyboardState::default(),
            gamepad: None,
            vibration: PadVibration { left: 0, right: 0 },
        }
    }

    /// A key event for the keyboard pad; unmapped keys pass through unheard.
    pub(crate) fn key_event(&mut self, key: PhysicalKey, state: ElementState) {
        if let PhysicalKey::Code(code) = key {
            self.keyboard.key_event(code, state);
        }
    }

    /// Reads the browser's gamepads (WebAssembly only; elsewhere there are
    /// none) and keeps the newest standard-mapping sample.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn poll_gamepads(&mut self) {
        let read = read_standard_gamepad();
        let previous = self.gamepad.take();
        self.gamepad = match (read, previous) {
            (Some((slot, pad)), previous) => {
                let sample = pad.sample();
                let packet = match previous {
                    Some((previous_slot, packet, previous_sample)) => {
                        if previous_sample == sample && previous_slot == slot {
                            packet
                        } else {
                            packet.wrapping_add(1)
                        }
                    }
                    None => 1,
                };
                Some((slot, packet, sample))
            }
            (None, _) => None,
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn poll_gamepads(&mut self) {
        self.gamepad = None;
    }

    /// The sample slot zero shows this frame, and where it came from: the
    /// gamepad when one is connected, else the keyboard. The future browser
    /// session will read the same choice through `PadPlatform::get_state`.
    pub(crate) fn sample(&mut self, dt: f32) -> (XboxPadSample, PadSource) {
        if let Some((slot, _, gamepad)) = self.gamepad {
            (gamepad, PadSource::Gamepad { index: slot })
        } else {
            (self.keyboard.sample(dt), PadSource::Keyboard)
        }
    }

    /// The vibration the game last set, for the debug HUD.
    #[must_use]
    #[cfg_attr(
        all(not(target_arch = "wasm32"), not(test)),
        expect(
            dead_code,
            reason = "the browser's debug HUD only exists on WebAssembly"
        )
    )]
    pub(crate) fn vibration(&self) -> PadVibration {
        self.vibration
    }
}

impl Default for WebPadPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// A browser transport error, which this platform's sources cannot produce
/// beyond the capabilities flags the game never sends.
#[derive(Debug)]
pub(crate) struct UnsupportedCapabilitiesFlags(u32);
impl std::fmt::Display for UnsupportedCapabilitiesFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsupported native controller capability flags {}",
            self.0
        )
    }
}
impl std::error::Error for UnsupportedCapabilitiesFlags {}

impl PadPlatform for WebPadPlatform {
    type Error = UnsupportedCapabilitiesFlags;

    fn get_state(&mut self, index: u32) -> Result<PadStateResult, Self::Error> {
        if index != 0 {
            return Ok(PadStateResult {
                status: DEVICE_NOT_CONNECTED,
                state: None,
            });
        }
        // A snapshot for the coming session path: the gamepad when one is
        // connected, else the keyboard's sample without advancing its ramps.
        let (packet, sample) = if let Some((slot, packet, gamepad)) = self.gamepad {
            let _ = slot;
            (packet, gamepad)
        } else {
            (KEYBOARD_PACKET, self.keyboard.sample(0.0))
        };
        Ok(PadStateResult {
            status: 0,
            state: Some((packet, sample)),
        })
    }

    fn get_capabilities(
        &mut self,
        index: u32,
        flags: u32,
    ) -> Result<PadCapabilitiesResult, Self::Error> {
        if flags != FLAG_GAMEPAD {
            return Err(UnsupportedCapabilitiesFlags(flags));
        }
        // Slot zero always has the keyboard behind it; as the SDL platform
        // shows its pads: type and subtype gamepad, no wireless flag.
        Ok(if index == 0 {
            PadCapabilitiesResult {
                status: 0,
                device_type: Some(1),
                subtype: Some(1),
                flags: Some(0),
            }
        } else {
            PadCapabilitiesResult {
                status: DEVICE_NOT_CONNECTED,
                device_type: None,
                subtype: None,
                flags: None,
            }
        })
    }

    fn set_state(&mut self, index: u32, vibration: PadVibration) -> Result<u32, Self::Error> {
        if index == 0 {
            self.vibration = vibration;
            return Ok(0);
        }
        Ok(DEVICE_NOT_CONNECTED)
    }
}

/// The first connected standard-mapping gamepad, as the browser reports it.
/// Non-standard mappings are left alone rather than guessed at.
#[cfg(target_arch = "wasm32")]
fn read_standard_gamepad() -> Option<(u32, StandardGamepad)> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    let pads = window.navigator().get_gamepads().ok()?;
    for slot in 0..pads.length() {
        let Some(pad) = pads.get(slot).dyn_into::<web_sys::Gamepad>().ok() else {
            continue;
        };
        if !pad.connected() || pad.mapping() != web_sys::GamepadMappingType::Standard {
            continue;
        }
        let buttons_array = pad.buttons();
        let axes_array = pad.axes();
        let entry = |array: &js_sys::Array, index: usize| {
            u32::try_from(index).ok().map(|index| array.get(index))
        };
        let mut buttons = [false; 16];
        let mut triggers = [0.0_f64; 2];
        for (index, button) in buttons.iter_mut().enumerate() {
            let value = entry(&buttons_array, index)
                .and_then(|value| value.dyn_ref::<web_sys::GamepadButton>().cloned())
                .map_or(0.0, |button| button.value());
            *button = value >= 0.5;
            if let Some(trigger) = index.checked_sub(6).filter(|trigger| *trigger < 2) {
                triggers[trigger] = value;
            }
        }
        let axis = |index: usize| {
            entry(&axes_array, index)
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        };
        // Firefox reports the triggers as axes 4/5 (-1..1) instead of button
        // values; whichever reports more, reports the trigger.
        if axes_array.length() >= 6 {
            for (trigger, axis_index) in [(0_usize, 4_usize), (1, 5)] {
                let axis_value = axis(axis_index).midpoint(1.0);
                triggers[trigger] = triggers[trigger].max(axis_value);
            }
        }
        let slot = pad.index();
        return Some((
            slot,
            StandardGamepad {
                buttons,
                triggers,
                axes: [axis(0), axis(1), axis(2), axis(3)],
            },
        ));
    }
    None
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
    fn keyboard_starts_and_stays_neutral() {
        let mut keyboard = KeyboardState::default();
        assert_eq!(keyboard.sample(1.0 / 60.0), neutral());
    }

    #[test]
    fn held_keys_ramp_to_full_deflection_and_back() {
        let mut keyboard = KeyboardState::default();
        keyboard.key_event(KeyCode::KeyW, ElementState::Pressed);
        keyboard.key_event(KeyCode::KeyD, ElementState::Pressed);
        let rising = keyboard.sample(1.0 / 60.0);
        assert!(
            rising.left_y > 0 && rising.left_y < i16::MAX,
            "a ramp rises gradually: {}",
            rising.left_y
        );
        keyboard.sample(0.1);
        let full = keyboard.sample(0.1);
        assert_eq!((full.left_y, full.left_x), (i16::MAX, i16::MAX));
        keyboard.key_event(KeyCode::KeyW, ElementState::Released);
        keyboard.key_event(KeyCode::KeyD, ElementState::Released);
        let falling = keyboard.sample(1.0 / 60.0);
        assert!(falling.left_y < full.left_y && falling.left_x < full.left_x);
        keyboard.sample(0.1);
        assert_eq!(keyboard.sample(0.1), neutral());
    }

    #[test]
    fn flicks_reach_full_deflection_faster_than_steering() {
        let mut flick = KeyboardState::default();
        flick.key_event(KeyCode::ArrowUp, ElementState::Pressed);
        flick.key_event(KeyCode::KeyW, ElementState::Pressed);
        let after_two_frames = flick.sample(1.0 / 30.0);
        assert!(
            after_two_frames.right_y > after_two_frames.left_y,
            "the flick stick outruns the steering stick: {} vs {}",
            after_two_frames.right_y,
            after_two_frames.left_y
        );
    }

    #[test]
    fn opposite_directions_hold_the_center() {
        let mut keyboard = KeyboardState::default();
        keyboard.key_event(KeyCode::KeyW, ElementState::Pressed);
        keyboard.key_event(KeyCode::KeyS, ElementState::Pressed);
        keyboard.sample(0.1);
        assert_eq!(keyboard.sample(0.1), neutral());
    }

    #[test]
    fn buttons_carry_the_xinput_bits() {
        let mut keyboard = KeyboardState::default();
        for key in [
            KeyCode::Space,
            KeyCode::ShiftLeft,
            KeyCode::KeyZ,
            KeyCode::KeyC,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Digit1,
            KeyCode::Digit2,
        ] {
            keyboard.key_event(key, ElementState::Pressed);
        }
        let buttons = keyboard.sample(0.0).buttons;
        assert_eq!(
            buttons,
            0x1000 | 0x2000 | 0x0100 | 0x0200 | 0x0010 | 0x0020 | 0x0040 | 0x0080
        );
        // The trigger keys ramp the analog channels, not the button bits.
        keyboard.key_event(KeyCode::KeyQ, ElementState::Pressed);
        keyboard.key_event(KeyCode::KeyE, ElementState::Pressed);
        keyboard.sample(0.1);
        let sample = keyboard.sample(0.1);
        assert_eq!(sample.buttons, buttons);
        assert_eq!((sample.left_trigger, sample.right_trigger), (255, 255));
    }

    #[test]
    fn standard_gamepad_maps_to_xinput_semantics() {
        let mut pad = StandardGamepad {
            buttons: [false; 16],
            triggers: [0.5, 1.0],
            axes: [1.0, -1.0, 0.0, 1.0],
        };
        // A, X, LB, D-Left pressed.
        pad.buttons[0] = true;
        pad.buttons[2] = true;
        pad.buttons[4] = true;
        pad.buttons[14] = true;
        let sample = pad.sample();
        assert_eq!(sample.buttons, 0x1000 | 0x4000 | 0x0100 | 0x0004);
        // Left stick fully right and up (browser Y down is negated).
        assert_eq!((sample.left_x, sample.left_y), (i16::MAX, i16::MAX));
        // Right stick fully down: -32767, the symmetric ±32767 scale the
        // native platform's converted axes also use, not i16::MIN.
        assert_eq!((sample.right_x, sample.right_y), (0, -i16::MAX));
        assert_eq!((sample.left_trigger, sample.right_trigger), (128, 255));
    }

    #[test]
    fn axis_conversion_is_symmetric_and_clamped() {
        assert_eq!(axis_i16(0.0), 0);
        assert_eq!(axis_i16(1.0), i16::MAX);
        assert_eq!(axis_i16(-1.0), -i16::MAX);
        assert_eq!(axis_i16(2.0), i16::MAX);
        assert_eq!(axis_i16(-2.0), -i16::MAX);
        assert_eq!(trigger_u8(0.0), 0);
        assert_eq!(trigger_u8(1.0), 255);
        assert_eq!(trigger_u8(9.0), 255);
    }

    #[test]
    fn platform_slots_read_like_xinput() {
        let mut platform = WebPadPlatform::new();
        let caps = platform.get_capabilities(0, FLAG_GAMEPAD).unwrap();
        assert_eq!(
            (caps.status, caps.device_type, caps.subtype, caps.flags),
            (0, Some(1), Some(1), Some(0))
        );
        let state = platform.get_state(0).unwrap();
        let (packet, sample) = state.state.expect("slot zero always has the keyboard");
        assert_eq!(
            (state.status, packet, sample),
            (0, KEYBOARD_PACKET, neutral())
        );
        for index in 1..4 {
            let state = platform.get_state(index).unwrap();
            assert_eq!(state.status, DEVICE_NOT_CONNECTED);
            assert!(state.state.is_none());
            let caps = platform.get_capabilities(index, FLAG_GAMEPAD).unwrap();
            assert_eq!(caps.status, DEVICE_NOT_CONNECTED);
            assert!(caps.device_type.is_none() && caps.subtype.is_none() && caps.flags.is_none());
        }
        assert_eq!(
            platform
                .set_state(
                    0,
                    PadVibration {
                        left: 100,
                        right: 200
                    }
                )
                .unwrap(),
            0
        );
        assert_eq!(
            platform.vibration(),
            PadVibration {
                left: 100,
                right: 200
            }
        );
        assert_eq!(
            platform
                .set_state(4, PadVibration { left: 0, right: 0 })
                .unwrap(),
            DEVICE_NOT_CONNECTED
        );
        assert!(platform.get_capabilities(0, 0).is_err());
    }
}
