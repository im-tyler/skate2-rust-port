//! The desktop debug binary for the browser test scene. The browser itself
//! starts through the library's WebAssembly entry (`start` in `lib.rs`).

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    skate2_web::run_native()
}

/// The browser loads the library's cdylib; the binary target is unused there.
#[cfg(target_arch = "wasm32")]
fn main() {}
