//! The 48 kHz stereo mix as an output device pulls it: [`AudioSystem::render_stereo`] blocks
//! queued and handed out one frame at a time, linearly interpolated when the device runs at
//! another rate. Shared by every host's output (the pc's cpal stream, the browser's Web Audio
//! pull), so the device sees the same frames everywhere.
//!
//! [`AudioSystem::render_stereo`]: crate::system::AudioSystem::render_stereo

use std::collections::VecDeque;

/// RWAC's mix rate.
pub const MIX_RATE: u32 = 48_000;

/// The mix-to-device frame converter.
#[derive(Debug)]
pub struct MixResampler {
    pending: VecDeque<f32>,
    block: Vec<f32>,
    /// The device runs at the mix rate: frames pass through untouched.
    passthrough: bool,
    step: f64,
    position: f64,
    previous: [f32; 2],
    current: [f32; 2],
}

impl MixResampler {
    /// A converter for a device running at `device_rate` frames per second.
    #[must_use]
    pub fn new(device_rate: u32) -> Self {
        Self {
            pending: VecDeque::new(),
            block: Vec::new(),
            passthrough: device_rate == MIX_RATE,
            step: f64::from(MIX_RATE) / f64::from(device_rate),
            position: 1.0,
            previous: [0.0; 2],
            current: [0.0; 2],
        }
    }

    /// Drops every queued and interpolated frame: the next frame starts a fresh block.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.previous = [0.0; 2];
        self.current = [0.0; 2];
        self.position = 1.0;
    }

    /// The next device frame. `render` appends one mix block (`render_stereo`) when the queue
    /// runs dry; when it answers `false` the frame is silence and nothing is queued.
    pub fn next_frame(&mut self, render: &mut impl FnMut(&mut Vec<f32>) -> bool) -> [f32; 2] {
        if self.passthrough {
            return self.next_mix_frame(render);
        }
        while self.position >= 1.0 {
            self.position -= 1.0;
            self.previous = self.current;
            self.current = self.next_mix_frame(render);
        }
        #[expect(clippy::cast_possible_truncation, reason = "position is in [0, 1)")]
        let t = self.position as f32;
        self.position += self.step;
        [
            self.previous[0] + (self.current[0] - self.previous[0]) * t,
            self.previous[1] + (self.current[1] - self.previous[1]) * t,
        ]
    }

    fn next_mix_frame(&mut self, render: &mut impl FnMut(&mut Vec<f32>) -> bool) -> [f32; 2] {
        if self.pending.len() < 2 {
            self.block.clear();
            if !render(&mut self.block) {
                return [0.0; 2];
            }
            self.pending.extend(self.block.iter().copied());
        }
        [
            self.pending.pop_front().unwrap_or(0.0),
            self.pending.pop_front().unwrap_or(0.0),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block source counting up: frame n is (n, -n).
    fn counter() -> impl FnMut(&mut Vec<f32>) -> bool {
        let mut next = 0.0_f32;
        move |block: &mut Vec<f32>| {
            for _ in 0..4 {
                block.extend([next, -next]);
                next += 1.0;
            }
            true
        }
    }

    #[test]
    fn at_the_mix_rate_frames_pass_through() {
        let mut resampler = MixResampler::new(MIX_RATE);
        let mut render = counter();
        let frames: Vec<_> = (0..6).map(|_| resampler.next_frame(&mut render)).collect();
        assert_eq!(frames[0], [0.0, -0.0]);
        assert_eq!(frames[5], [5.0, -5.0]);
    }

    #[test]
    fn at_another_rate_frames_interpolate_between_mix_frames() {
        // 24 kHz: two mix frames per device frame, t stays 0 — every other mix frame.
        let mut resampler = MixResampler::new(24_000);
        let mut render = counter();
        let frames: Vec<_> = (0..3).map(|_| resampler.next_frame(&mut render)).collect();
        assert_eq!(frames, [[0.0, 0.0], [1.0, -1.0], [3.0, -3.0]]);
        // 96 kHz: half steps.
        let mut resampler = MixResampler::new(96_000);
        let mut render = counter();
        let frames: Vec<_> = (0..4).map(|_| resampler.next_frame(&mut render)).collect();
        assert_eq!(frames, [[0.0, 0.0], [0.0, 0.0], [0.0, -0.0], [0.5, -0.5]]);
    }

    #[test]
    fn a_refused_render_is_silence_and_queues_nothing() {
        let mut resampler = MixResampler::new(MIX_RATE);
        assert_eq!(resampler.next_frame(&mut |_| false), [0.0; 2]);
        let mut render = counter();
        assert_eq!(resampler.next_frame(&mut render), [0.0, -0.0]);
    }
}
