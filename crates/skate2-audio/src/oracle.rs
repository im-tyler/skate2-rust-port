//! The pinned audio render oracle (`docs/WEB_PORT_M6_PLAN.md`, phase 1c).
//!
//! The native render is not reproducible as the game runs it: three seeds are
//! drawn from the clock (phase 1a made them inputs), and the music stream's
//! reader thread races the render when it outruns real time — measured as a
//! moving render even with the clock pinned under load. This module removes
//! both by construction: the seeds are fixed constants, and the stream files
//! are **absent** — every stream consumer (`speech::instance::load`,
//! `load_resident_data`, the pool's `read_chunk`) answers a missing file with
//! an early return, and the pool's reader thread drops the failed request
//! without retrying, so no bytes are ever read and nothing races. The
//! pause-menu press sound is a splicer bank sound and renders identically
//! with the streams gone; the hash pins exactly that path. A tab that runs
//! the same script over the same absent streams — where no reader thread can
//! exist at all — produces the same frames (`docs/WEB_PORT_M6_PLAN.md`
//! phase 2).

use crate::rwac::stream_pool::StreamFiles;
use crate::system::{AudioSystem, GameAudioState, SystemSeeds, hud_state};

/// The oracle's seeds: arbitrary fixed constants standing in for `InitApp`'s
/// `mftb`, `SPCH_Init`'s `time64` and the speech context draw. Recorded once;
/// changing them changes the oracle hash.
pub const ORACLE_SEEDS: SystemSeeds = SystemSeeds {
    music: 0x0cba_5eed,
    speech: 0x5eed_0cba,
    contexts: 0xa5e3_d0c5,
};

/// The oracle's stream files: nothing exists. Every read fails at `size`, so
/// the speech projects load empty, the resident samples stay absent, and the
/// pool's streams never hold a chunk — on any host, threaded or not.
pub struct AbsentStreams;

impl StreamFiles for AbsentStreams {
    fn size(&self, _path: &str) -> Option<u64> {
        None
    }

    fn read_at(&self, _path: &str, _offset: u64, _buf: &mut [u8]) -> bool {
        false
    }
}

/// FNV-1a over the frames' raw `f32` bits (little-endian): a stable hash of a
/// render that must be bit-identical across runs and hosts.
#[must_use]
pub fn frame_hash(frames: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for frame in frames {
        for byte in frame.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// The oracle script: the pause-menu press sound over 180 ticks of free-skate
/// state — `Audio.PlaySound(FE_SOUND_PAUSE_MENU_PRESS)`, then the same
/// update/render cadence the pc corpus test drives (one update per 60 Hz tick,
/// 256-frame blocks until the tick's boundary). Returns the rendered frames'
/// hash. The system must have been built with [`ORACLE_SEEDS`] and
/// [`AbsentStreams`].
pub fn pause_menu_press_render(system: &mut AudioSystem, key: u64) -> u64 {
    system.state = GameAudioState {
        hud_state: hud_state::FREE_SKATE,
        ..GameAudioState::default()
    };
    system.global_fe_play_sound(key, false);
    let mut stereo = Vec::new();
    let mut mixed = 0.0_f64;
    for tick in 0..180 {
        system.update(1.0 / 60.0);
        while mixed < f64::from(tick + 1) / 60.0 {
            system.render_stereo(&mut stereo);
            mixed += 256.0 / 48_000.0;
        }
    }
    frame_hash(&stereo)
}
