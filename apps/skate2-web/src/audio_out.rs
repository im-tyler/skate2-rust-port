//! Milestone six's audible step (`docs/WEB_PORT_M6_PLAN.md`, phase 3): the
//! pause-menu press sound through Web Audio.
//!
//! A `ScriptProcessorNode` pulls device frames on the main thread — the
//! thread the single-threaded wasm `AudioSystem` lives on — through
//! `skate2_audio::output::MixResampler`, the converter the pc's cpal stream
//! uses, so the device sees the same frames. A 60 Hz `setInterval` drives
//! `update(1/60)` (a queued sound plays only once updates run), catching up
//! by whole ticks against the page clock.
//!
//! The tradeoff: `ScriptProcessorNode` is deprecated and runs on the main
//! thread, so a long main-thread task delays a pull and the block plays late
//! (counted below as `late`). The follow-up is an `AudioWorklet` fed through
//! a `SharedArrayBuffer` ring, which needs cross-origin isolation (COOP/COEP)
//! and the system on a worker; it is recorded in `docs/WEB_PORT.md`, not
//! built here.
//!
//! The `AudioContext` starts suspended until a user gesture: the play button
//! is that gesture. Headless verification reads the pull counters (blocks,
//! peak, late) with Chrome's autoplay gesture requirement lifted;
//! audibility itself is verified by hand.

use crate::audio_probe::{self, seconds_now};
use skate2_audio::{output::MixResampler, system::AudioSystem};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};

/// Frames per pull: 2048 at 48 kHz is 43 ms of buffering — enough slack for
/// main-thread jitter, at that much latency.
const PULL_FRAMES: u32 = 2048;

/// What the pull has done, for the panel.
#[derive(Default)]
struct Counters {
    blocks: Cell<u32>,
    peak: Cell<f32>,
    /// Pulls whose block was already due when the callback ran: an audible gap.
    late: Cell<u32>,
    presses: Cell<u32>,
}

/// The live player: the context, the system, and the callbacks keeping it
/// running. Dropping it would stop the pull, so it lives for the page.
struct Player {
    context: web_sys::AudioContext,
    system: Rc<RefCell<AudioSystem>>,
    key: u64,
    counters: Rc<Counters>,
    _node: web_sys::ScriptProcessorNode,
    _pull: Closure<dyn FnMut(web_sys::AudioProcessingEvent)>,
    _tick: Closure<dyn FnMut()>,
}

enum State {
    Idle,
    Loading,
    Ready(Rc<Player>),
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
}

/// The play button: the first press creates and resumes the context inside
/// the gesture, fetches and builds the system, starts the pull and plays the
/// sound; later presses play it again.
pub(crate) fn press<S: crate::game_files::ByteSource + 'static>(
    source: impl FnOnce() -> S,
    line: web_sys::Element,
) {
    let ready = STATE.with(|state| match &*state.borrow() {
        State::Ready(player) => Some(Rc::clone(player)),
        _ => None,
    });
    if let Some(player) = ready {
        let _ = player.context.resume();
        play(&player);
        return;
    }
    if STATE.with(|state| matches!(&*state.borrow(), State::Loading)) {
        return;
    }
    // Created inside the gesture, so the browser lets it run.
    let context = match web_sys::AudioContext::new() {
        Ok(context) => context,
        Err(error) => {
            line.set_text_content(Some(&format!("audio out: no AudioContext: {error:?}")));
            return;
        }
    };
    let _ = context.resume();
    STATE.with(|state| *state.borrow_mut() = State::Loading);
    let source = source();
    wasm_bindgen_futures::spawn_local(async move {
        let started = match load(&source, context, &line).await {
            Ok(player) => {
                play(&player);
                State::Ready(player)
            }
            Err(problem) => {
                line.set_text_content(Some(&format!("audio out: {problem}")));
                State::Idle
            }
        };
        STATE.with(|state| *state.borrow_mut() = started);
    });
}

fn play(player: &Player) {
    skate2_audio::oracle::pause_menu_press(&mut player.system.borrow_mut(), player.key);
    player
        .counters
        .presses
        .set(player.counters.presses.get() + 1);
}

async fn load<S: crate::game_files::ByteSource>(
    source: &S,
    context: web_sys::AudioContext,
    line: &web_sys::Element,
) -> Result<Rc<Player>, String> {
    let (bytes, _) = audio_probe::fetch(source, line).await?;
    let built = audio_probe::build(&bytes)?;
    let system = Rc::new(RefCell::new(built.system));
    let counters = Rc::new(Counters::default());
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sample rates are small positive integers"
    )]
    let rate = context.sample_rate() as u32;
    let node = context
        .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(
            PULL_FRAMES,
            0,
            2,
        )
        .map_err(|error| format!("no ScriptProcessorNode: {error:?}"))?;
    let pull = pull_callback(&system, &counters, &context, rate);
    node.set_onaudioprocess(Some(pull.as_ref().unchecked_ref()));
    node.connect_with_audio_node(&context.destination())
        .map_err(|error| format!("connect: {error:?}"))?;
    let tick = {
        let system = Rc::clone(&system);
        let counters = Rc::clone(&counters);
        let context = context.clone();
        let line = line.clone();
        let start = seconds_now();
        let mut ticks = 0_u64;
        let mut firings = 0_u32;
        Closure::<dyn FnMut()>::new(move || {
            // Whole 60 Hz ticks against the page clock, at most a few per
            // timer firing: a throttled timer catches up, a stalled tab does
            // not replay seconds of updates at once.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "elapsed page time is small and non-negative"
            )]
            let due = ((seconds_now() - start) * 60.0) as u64;
            let mut system = system.borrow_mut();
            for _ in 0..due.saturating_sub(ticks).min(4) {
                system.update(1.0 / 60.0);
            }
            ticks = due;
            firings = firings.wrapping_add(1);
            if firings.is_multiple_of(15) {
                line.set_text_content(Some(&panel_line(&context, rate, &counters)));
            }
        })
    };
    web_sys::window()
        .ok_or("no Window")?
        .set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            1000 / 60,
        )
        .map_err(|error| format!("setInterval: {error:?}"))?;
    Ok(Rc::new(Player {
        context,
        system,
        key: built.key,
        counters,
        _node: node,
        _pull: pull,
        _tick: tick,
    }))
}

/// The `onaudioprocess` pull: one device block through the resampler, the
/// counters updated.
fn pull_callback(
    system: &Rc<RefCell<AudioSystem>>,
    counters: &Rc<Counters>,
    context: &web_sys::AudioContext,
    rate: u32,
) -> Closure<dyn FnMut(web_sys::AudioProcessingEvent)> {
    let system = Rc::clone(system);
    let counters = Rc::clone(counters);
    let context = context.clone();
    let mut resampler = MixResampler::new(rate);
    let mut left = vec![0.0_f32; PULL_FRAMES as usize];
    let mut right = vec![0.0_f32; PULL_FRAMES as usize];
    Closure::<dyn FnMut(web_sys::AudioProcessingEvent)>::new(
        move |event: web_sys::AudioProcessingEvent| {
            if event.playback_time() < context.current_time() {
                counters.late.set(counters.late.get() + 1);
            }
            let Ok(buffer) = event.output_buffer() else {
                return;
            };
            let frames = buffer.length() as usize;
            left.resize(frames, 0.0);
            right.resize(frames, 0.0);
            let mut system = system.borrow_mut();
            let mut render = |block: &mut Vec<f32>| {
                system.render_stereo(block);
                true
            };
            let mut peak = counters.peak.get();
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let [a, b] = resampler.next_frame(&mut render);
                *l = a.clamp(-1.0, 1.0);
                *r = b.clamp(-1.0, 1.0);
                peak = peak.max(a.abs()).max(b.abs());
            }
            counters.peak.set(peak);
            let _ = buffer.copy_to_channel(&left, 0);
            let _ = buffer.copy_to_channel(&right, 1);
            counters.blocks.set(counters.blocks.get() + 1);
        },
    )
}

fn panel_line(context: &web_sys::AudioContext, rate: u32, counters: &Counters) -> String {
    format!(
        "audio out: {:?} at {rate} Hz ({PULL_FRAMES}-frame main-thread pulls); {} blocks pulled, peak {:.4}, {} late; {} presses",
        context.state(),
        counters.blocks.get(),
        counters.peak.get(),
        counters.late.get(),
        counters.presses.get()
    )
}
