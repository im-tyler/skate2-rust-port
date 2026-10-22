//! Browser (WebAssembly + WebGPU) build of the Skate 2 Rust port: the first
//! pass described in `docs/WEB_PORT.md`.
//!
//! What this crate is now: a WASM target that compiles `skate2-core`, renders
//! a standalone test scene through wgpu's WebGPU backend (WebGL2 as the
//! fallback), and reads the keyboard and the browser's Gamepad API through the
//! same `PadPlatform` boundary the native builds use, producing
//! `XboxPadSample`s.
//!
//! What it deliberately is not yet: the real Skate 2 renderer, the
//! reconstructed session, or any way to read the original game's files in a
//! browser. Those, and the blockers on their way, are listed in
//! `docs/WEB_PORT.md`.

mod input;
mod scene;

#[cfg(target_arch = "wasm32")]
mod game_files;
#[cfg(target_arch = "wasm32")]
mod vault;

use input::{PadSource, WebPadPlatform};
use scene::{PlazaScene, PresentProblem};
use skate2_core::XboxPadSample;
use std::sync::Arc;
use winit::{
    event::{Event, WindowEvent},
    event_loop::ControlFlow,
    window::Window,
};

/// The canvas the browser build attaches winit to (`index.html`).
#[cfg(target_arch = "wasm32")]
const CANVAS_ID: &str = "skate2-canvas";

/// Frame timing: `Instant` on the desktop, the browser's high-resolution
/// clock on WebAssembly, where `Instant` is not implemented.
struct Clock {
    #[cfg(not(target_arch = "wasm32"))]
    last: Option<std::time::Instant>,
    #[cfg(target_arch = "wasm32")]
    last: Option<f64>,
}

impl Clock {
    fn new() -> Self {
        Self { last: None }
    }

    /// Seconds since the last tick; the first frame assumes one 60 Hz frame,
    /// and a stalled frame is clamped so a hidden tab cannot jump the scene.
    fn tick(&mut self) -> f32 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let now = std::time::Instant::now();
            let seconds = self.last.map_or(1.0 / 60.0, |last| {
                (now - last).as_secs_f32().clamp(0.0, 0.1)
            });
            self.last = Some(now);
            seconds
        }
        #[cfg(target_arch = "wasm32")]
        {
            let now = seconds_now();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a frame delta far below f32 precision is all the clock is for"
            )]
            let seconds = self
                .last
                .map_or(1.0 / 60.0, |last| ((now - last) as f32).clamp(0.0, 0.1));
            self.last = Some(now);
            seconds
        }
    }
}

/// The browser's clock, in seconds.
#[cfg(target_arch = "wasm32")]
fn seconds_now() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or_else(
            || js_sys::Date::now() / 1000.0,
            |performance| performance.now() / 1000.0,
        )
}

/// The app: one window, the test scene, the pad platform and the debug HUD.
pub(crate) struct App {
    window: Arc<Window>,
    scene: Option<PlazaScene>,
    pad: WebPadPlatform,
    clock: Clock,
    hud: Hud,
    /// Whether the scene already failed fatally, so it is reported once.
    fatal: bool,
}

impl App {
    pub(crate) fn new(window: Arc<Window>, scene: PlazaScene) -> Self {
        Self {
            window,
            scene: Some(scene),
            pad: WebPadPlatform::new(),
            clock: Clock::new(),
            hud: Hud::new(),
            fatal: false,
        }
    }

    /// One winit event, on the desktop's `run` and the browser's `spawn`.
    pub(crate) fn handle_event(
        &mut self,
        event: Event<()>,
        target: &winit::event_loop::EventLoopWindowTarget<()>,
    ) {
        target.set_control_flow(ControlFlow::Wait);
        let Event::WindowEvent { event, .. } = event else {
            return;
        };
        match event {
            WindowEvent::Resized(size) => {
                if let Some(scene) = &mut self.scene {
                    scene.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.pad.key_event(event.physical_key, event.state);
            }
            WindowEvent::CloseRequested => target.exit(),
            WindowEvent::RedrawRequested => self.frame(),
            _ => {}
        }
    }

    /// One frame: read the pads, advance and draw the scene, update the HUD.
    fn frame(&mut self) {
        let dt = self.clock.tick();
        self.pad.poll_gamepads();
        let (sample, source) = self.pad.sample(dt);
        if let Some(scene) = &mut self.scene {
            match scene.render(sample, dt) {
                Err(PresentProblem::Fatal) if !self.fatal => {
                    self.fatal = true;
                    note("the GPU rejected the frame (a wgpu validation error); see the console");
                }
                Ok(()) | Err(PresentProblem::Skip | PresentProblem::Fatal) => {}
                Err(PresentProblem::Reconfigure) => {
                    let size = self.window.inner_size();
                    scene.resize(size.width, size.height);
                }
            }
        }
        let backend = self.scene.as_ref().map_or("", PlazaScene::adapter_backend);
        self.hud.update(&self.pad, sample, source, backend, dt);
        self.window.request_redraw();
    }
}

/// The desktop debug entry: the same test scene in a native window, where
/// blocking on wgpu's asynchronous setup is allowed. `cargo run -p skate2-web`
/// from the repository root.
///
/// # Errors
///
/// The event loop, window or GPU setup failed.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = winit::event_loop::EventLoop::new()?;
    let window = Arc::new(
        winit::window::WindowBuilder::new()
            .with_title("Skate 2 — browser test scene (native debug)")
            .build(&event_loop)?,
    );
    let scene = pollster::block_on(PlazaScene::new(window.clone()))?;
    let mut app = App::new(window, scene);
    Hud::ready();
    event_loop.run(move |event, target| {
        app.handle_event(event, target);
    })?;
    Ok(())
}

/// The browser entry: the WebAssembly module's start. Finds the canvas,
/// checks for WebGPU, prepares winit's event loop, then initialises wgpu
/// asynchronously — `Instant` and blocking pools do not exist on this target —
/// before the frame loop starts.
///
/// # Errors
///
/// The canvas or browser APIs are missing, or winit's loop could not start.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    use winit::platform::web::WindowBuilderExtWebSys;

    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or_else(|| js_value("no Window"))?;
    let document = window.document().ok_or_else(|| js_value("no Document"))?;
    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .ok_or_else(|| js_value("the #skate2-canvas canvas is missing"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| js_value("#skate2-canvas is not a canvas"))?;
    // The game-data panel arms independently of the GPU: it works in every
    // browser, with or without WebGPU.
    arm_assets_panel();
    // Say it plainly before wgpu's adapter request fails obscurely.
    let gpu = js_sys::Reflect::get(
        &(window.navigator().into()),
        &wasm_bindgen::JsValue::from("gpu"),
    )?;
    if gpu.is_undefined() {
        note(
            "This browser has no WebGPU. Try Chrome or Edge 113+, Safari 18+, or Firefox with WebGPU enabled.",
        );
        return Ok(());
    }
    let event_loop = winit::event_loop::EventLoopBuilder::new()
        .build()
        .map_err(|error| js_value(&error.to_string()))?;
    let window = Arc::new(
        winit::window::WindowBuilder::new()
            .with_canvas(Some(canvas))
            .build(&event_loop)
            .map_err(|error| js_value(&error.to_string()))?,
    );
    wasm_bindgen_futures::spawn_local(async move {
        match PlazaScene::new(Arc::clone(&window)).await {
            Ok(scene) => spawn_event_loop(event_loop, window, scene),
            Err(error) => note(&format!("The GPU could not be initialised: {error}")),
        }
    });
    Ok(())
}

/// Runs the app on the browser's event loop once the GPU is ready.
#[cfg(target_arch = "wasm32")]
fn spawn_event_loop(
    event_loop: winit::event_loop::EventLoop<()>,
    window: Arc<Window>,
    scene: PlazaScene,
) {
    use winit::platform::web::EventLoopExtWebSys;
    let mut app = App::new(window, scene);
    app.hud.ready();
    event_loop.spawn(move |event, target| {
        app.handle_event(event, target);
    });
}

/// Arms the game-data panel (`docs/WEB_PORT.md`, asset pipeline step one):
/// the `?http=<base>` source when the page carries one, and a folder picker
/// that keeps the player's dump in their browser. The event closures live as
/// long as the page through the keepalive below — no `forget()` leaks.
#[cfg(target_arch = "wasm32")]
fn arm_assets_panel() {
    use game_files::{HttpFiles, PickedFiles};
    use wasm_bindgen::{JsCast, closure::Closure};
    /// One panel event listener, kept for the page's lifetime.
    type PanelClosure = Closure<dyn FnMut(web_sys::Event)>;
    thread_local! {
        /// The panel's closures, kept alive for the page's lifetime.
        static KEEP: std::cell::RefCell<Vec<PanelClosure>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let search = window.location().search().unwrap_or_default();
    if let Some(base) = search
        .strip_prefix('?')
        .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("http=")))
    {
        wasm_bindgen_futures::spawn_local(run_assets_report(HttpFiles::new(base)));
    }
    let Some(input) = document
        .get_element_by_id("game-folder")
        .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    let Some(button) = document.get_element_by_id("pick-folder") else {
        return;
    };
    let open = {
        let input = input.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
            input.click();
        })
    };
    button
        .add_event_listener_with_callback("click", open.as_ref().unchecked_ref())
        .ok();
    let chosen = Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
        let Some(input) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("game-folder"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        match PickedFiles::from_input(&input) {
            Ok(files) => {
                note(&format!("reading the dump: {} files picked…", files.len()));
                wasm_bindgen_futures::spawn_local(run_assets_report(files));
            }
            Err(problem) => note(&problem),
        }
    });
    input
        .add_event_listener_with_callback("change", chosen.as_ref().unchecked_ref())
        .ok();
    KEEP.with(|keep| {
        keep.borrow_mut().extend([open, chosen]);
    });
}

/// One source's report, as the panel's lines.
#[cfg(target_arch = "wasm32")]
async fn run_assets_report<S: game_files::ByteSource>(source: S) {
    use game_files::AssetsReport;
    let AssetsReport {
        found,
        missing,
        parsed,
        entry_read,
        vault_line,
    } = game_files::inspect(&source).await;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let panel = document.get_element_by_id("assets");
    let found_line = document.get_element_by_id("assets-found");
    let archives = document.get_element_by_id("assets-archives");
    let row = |text: &str| -> Option<web_sys::Node> {
        let element = document.create_element("div").ok()?;
        element.set_text_content(Some(text));
        Some(element.into())
    };
    if let Some(found_line) = found_line {
        found_line.set_text_content(Some(&format!("{found}/20 archives")));
    }
    if let Some(archives) = archives {
        while let Some(child) = archives.first_child() {
            archives.remove_child(&child).ok();
        }
        if !missing.is_empty() {
            if let Some(node) = row(&format!("missing: {}", missing.join(", "))) {
                archives.append_child(&node).ok();
            }
        }
        for archive in parsed {
            let entries = if archive.entries > 0 {
                format!("{} entries", archive.entries)
            } else {
                "no entries".to_owned()
            };
            let line = format!(
                "{} — {entries}: {}",
                archive.path,
                archive.sample.join(" · ")
            );
            if let Some(node) = row(&line) {
                archives.append_child(&node).ok();
            }
        }
        // Step two's proof line: the ranged entry read.
        if let Some(node) = row(&format!("ranged read: {entry_read}")) {
            archives.append_child(&node).ok();
        }
        // Step three's proof line: the Vault database itself.
        if let Some(node) = row(&format!("vault: {vault_line}")) {
            archives.append_child(&node).ok();
        }
    }
    if let Some(panel) = panel {
        panel.set_attribute("style", "display: block").ok();
    }
}

/// A `JsValue` error from a message.
#[cfg(target_arch = "wasm32")]
fn js_value(message: &str) -> wasm_bindgen::JsValue {
    js_sys::Error::new(message).into()
}

/// Puts a message where the player will see it: the loading panel in the
/// browser, stderr on the desktop.
fn note(message: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::console::error_1(&message.into());
        let text = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("loading-text"));
        if let Some(text) = text {
            text.set_text_content(Some(message));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}

/// The debug HUD: the live pad sample, the adapter and the frame rate. In the
/// browser this is the HTML panel from `index.html`; on the desktop, where
/// there is no page, it is nothing.
struct Hud {
    #[cfg(target_arch = "wasm32")]
    elements: Option<HudElements>,
    #[cfg(target_arch = "wasm32")]
    frames: (u32, f64),
    #[cfg(not(target_arch = "wasm32"))]
    _desktop: (),
}

/// The HUD's elements, fetched once; any that are missing are simply skipped.
#[cfg(target_arch = "wasm32")]
struct HudElements {
    source: Option<web_sys::Element>,
    buttons: Option<web_sys::Element>,
    backend: Option<web_sys::Element>,
    fps: Option<web_sys::Element>,
    vibration: Option<web_sys::Element>,
    sticks: [Option<web_sys::Element>; 4],
    triggers: [Option<web_sys::Element>; 2],
}

/// Sets an element's text when it exists.
#[cfg(target_arch = "wasm32")]
fn set_text(element: Option<&web_sys::Element>, text: &str) {
    if let Some(element) = element {
        element.set_text_content(Some(text));
    }
}

/// Sets an element's style when it exists.
#[cfg(target_arch = "wasm32")]
fn set_style(element: Option<&web_sys::Element>, style: &str) {
    if let Some(element) = element {
        element.set_attribute("style", style).ok();
    }
}

impl Hud {
    fn new() -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            elements: None,
            #[cfg(target_arch = "wasm32")]
            frames: (0, 0.0),
            #[cfg(not(target_arch = "wasm32"))]
            _desktop: (),
        }
    }

    /// Fetches the HUD's elements, hides the loading panel and shows the HUD.
    #[cfg(target_arch = "wasm32")]
    fn ready(&mut self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let element = |id: &str| document.get_element_by_id(id);
        self.elements = Some(HudElements {
            source: element("hud-source"),
            buttons: element("hud-buttons"),
            backend: element("hud-backend"),
            fps: element("hud-fps"),
            vibration: element("hud-vib"),
            sticks: [
                element("bar-lx"),
                element("bar-ly"),
                element("bar-rx"),
                element("bar-ry"),
            ],
            triggers: [element("bar-lt"), element("bar-rt")],
        });
        set_style(element("hud").as_ref(), "display: block");
        set_style(element("loading").as_ref(), "display: none");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ready() {}

    /// Refreshes the HUD from the latest sample. On the desktop this does
    /// nothing; the sample itself is the same either way.
    fn update(
        &mut self,
        pad: &WebPadPlatform,
        sample: XboxPadSample,
        source: PadSource,
        backend: &str,
        dt: f32,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            let Some(elements) = &self.elements else {
                return;
            };
            set_text(
                elements.source.as_ref(),
                &match source {
                    PadSource::Keyboard => "keyboard".to_owned(),
                    PadSource::Gamepad { index } => format!("gamepad {index}"),
                },
            );
            set_text(
                elements.buttons.as_ref(),
                &format!("0x{:04x}", sample.buttons),
            );
            set_text(elements.backend.as_ref(), backend);
            let vibration = pad.vibration();
            set_text(
                elements.vibration.as_ref(),
                &format!("{} / {}", vibration.left, vibration.right),
            );
            let sticks = [
                f32::from(sample.left_x) / 32767.0,
                f32::from(sample.left_y) / 32767.0,
                f32::from(sample.right_x) / 32767.0,
                f32::from(sample.right_y) / 32767.0,
            ];
            for (bar, value) in elements.sticks.iter().zip(sticks) {
                set_style(
                    bar.as_ref(),
                    &format!(
                        "width: {:.0}%; background: {}",
                        value.abs() * 100.0,
                        if value < 0.0 { "#e0a03c" } else { "#4ca8e0" }
                    ),
                );
            }
            let triggers = [
                f32::from(sample.left_trigger) / 255.0,
                f32::from(sample.right_trigger) / 255.0,
            ];
            for (bar, value) in elements.triggers.iter().zip(triggers) {
                set_style(bar.as_ref(), &format!("width: {:.0}%", value * 100.0));
            }
            self.frames.0 += 1;
            self.frames.1 += f64::from(dt);
            if self.frames.0 >= 30 {
                let fps = f64::from(self.frames.0) / self.frames.1.max(1.0e-6);
                set_text(elements.fps.as_ref(), &format!("{fps:.0}"));
                self.frames = (0, 0.0);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = self;
            let _ = (pad, sample, source, backend, dt);
        }
    }
}
