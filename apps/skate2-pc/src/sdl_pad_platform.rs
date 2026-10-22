//! The pads where Windows' pad API does not exist (Linux): every pad through SDL3
//! (`crate::sdl_pads`), each reported as `XInputGetState` and `XInputGetCapabilities` report a
//! wired Xbox 360 gamepad. Native mapping, polling cadence and caches live in core.
//! When no SDL gamepad is connected, the keyboard stands in as slot zero
//! (`crate::keyboard_pads`, PC port addition), as the Skate 3 browser port's
//! loader does: slot zero always answers, with the keyboard's digital sample.
use crate::keyboard_pads::KeyboardPad;
use crate::sdl_pads::SdlPads;
use skate2_core::{
    PadCapabilitiesResult, PadPlatform, PadStateResult, PadVibration, XboxPadSample,
};
use winit::{event::ElementState, keyboard::PhysicalKey};

/// `ERROR_DEVICE_NOT_CONNECTED`: what `XInputGetState` returns for a slot with no pad.
const DEVICE_NOT_CONNECTED: u32 = 1167;
/// `XINPUT_FLAG_GAMEPAD`, the capabilities the game asks for.
const FLAG_GAMEPAD: u32 = 1;

pub(crate) struct SdlPadPlatform {
    /// Explicit diagnostic replay sample for controller zero; None reads SDL.
    pub(crate) demo_sample: Option<XboxPadSample>,
    sdl: Option<SdlPads>,
    /// The keyboard behind slot zero when no SDL gamepad is connected.
    keyboard: KeyboardPad,
}
impl SdlPadPlatform {
    pub(crate) fn new(demo_sample: Option<XboxPadSample>, sdl: Option<SdlPads>) -> Self {
        Self {
            demo_sample,
            sdl,
            keyboard: KeyboardPad::default(),
        }
    }

    /// One keyboard event for the fallback pad; text entry clears it instead.
    pub(crate) fn keyboard_key(&mut self, key: PhysicalKey, state: ElementState) {
        self.keyboard.key_event(key, state);
    }

    /// Releases the fallback pad's keys, when focus or the keyboard leaves it.
    pub(crate) fn keyboard_clear(&mut self) {
        self.keyboard.clear();
    }
}
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

impl PadPlatform for SdlPadPlatform {
    type Error = UnsupportedCapabilitiesFlags;
    fn get_state(&mut self, index: u32) -> Result<PadStateResult, Self::Error> {
        if index == 0 {
            if let Some(sample) = self.demo_sample {
                return Ok(PadStateResult {
                    status: 0,
                    state: Some((7, sample)),
                });
            }
        }
        let Some((packet, sample)) = self.sdl.as_mut().and_then(|sdl| sdl.claim(index)) else {
            // No SDL gamepad in this slot: the keyboard answers for slot zero,
            // as the Skate 3 browser port's loader does.
            if index == 0 {
                let (packet, sample) = self.keyboard.claim();
                if std::env::var_os("SKATE2_TRACE_XINPUT").is_some() {
                    eprintln!("keyboard slot=0 packet={packet} sample={sample:?}");
                }
                return Ok(PadStateResult {
                    status: 0,
                    state: Some((packet, sample)),
                });
            }
            return Ok(PadStateResult {
                status: DEVICE_NOT_CONNECTED,
                state: None,
            });
        };
        if index == 0 && std::env::var_os("SKATE2_TRACE_XINPUT").is_some() {
            eprintln!("sdl slot=0 packet={packet} sample={sample:?}");
        }
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
        // A diagnostic sample advertises the original-data pad corpus's controller-0
        // capabilities, as on Windows.
        if index == 0 && self.demo_sample.is_some() {
            return Ok(PadCapabilitiesResult {
                status: 0,
                device_type: Some(1),
                subtype: Some(0),
                flags: Some(0),
            });
        }
        // As DS4Windows showed its pads on Windows: type and subtype gamepad, no wireless flag.
        // Slot zero is always answered — by the keyboard when no gamepad is connected.
        Ok(if index == 0
            || self.sdl.as_mut().and_then(|sdl| sdl.claim(index)).is_some()
        {
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
        Ok(if self.sdl.as_mut().is_some_and(|sdl| sdl.vibrate(index, vibration)) {
            0
        } else if index == 0 {
            // The keyboard accepts vibration like the browser pad does: kept,
            // but there is nothing to rumble.
            0
        } else {
            DEVICE_NOT_CONNECTED
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_answers_slot_zero_and_the_rest_stay_empty() {
        let mut platform = SdlPadPlatform::new(None, None);
        // Slot zero: capabilities as a wired gamepad, a neutral keyboard sample.
        let caps = platform.get_capabilities(0, FLAG_GAMEPAD).unwrap();
        assert_eq!(
            (caps.status, caps.device_type, caps.subtype, caps.flags),
            (0, Some(1), Some(1), Some(0))
        );
        let state = platform.get_state(0).unwrap();
        let (packet, sample) = state.state.expect("the keyboard is always present");
        assert_eq!((state.status, packet, sample.buttons), (0, 1, 0));
        assert_eq!(platform.set_state(0, PadVibration { left: 100, right: 100 }).unwrap(), 0);
        // The other slots read as XInput's empty slots.
        for index in 1..4 {
            let state = platform.get_state(index).unwrap();
            assert_eq!((state.status, state.state.is_some()), (DEVICE_NOT_CONNECTED, false));
            let caps = platform.get_capabilities(index, FLAG_GAMEPAD).unwrap();
            assert_eq!(caps.status, DEVICE_NOT_CONNECTED);
            assert!(caps.device_type.is_none() && caps.subtype.is_none() && caps.flags.is_none());
            let vibration = PadVibration { left: 0, right: 0 };
            assert_eq!(platform.set_state(index, vibration).unwrap(), DEVICE_NOT_CONNECTED);
        }
        assert!(platform.get_capabilities(0, 0).is_err());
        // Keyboard events reach the sample through the platform.
        platform.keyboard_key(
            winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Space),
            ElementState::Pressed,
        );
        let state = platform.get_state(0).unwrap();
        let (_, sample) = state.state.expect("slot zero still present");
        assert_eq!(sample.buttons, 0x1000);
        platform.keyboard_clear();
        let state = platform.get_state(0).unwrap();
        assert_eq!(state.state.expect("slot zero still present").1.buttons, 0);
    }
}
