#![allow(unused_imports)]

mod api;
mod memory;
mod plugins;
mod runner;
mod sink;
mod types;
mod validation;

#[cfg(test)]
mod tests;

pub use api::*;
pub use types::*;

/// Install the `console_error_panic_hook` so that panics in WASM print the
/// real message + source location to the JS console instead of the opaque
/// `unreachable executed` trap string.
///
/// Idempotent via `Once` — safe to call from every `#[wasm_bindgen]` entry
/// point.  No-op on non-wasm32 targets (native tests).
#[cfg(target_arch = "wasm32")]
pub(crate) fn ensure_panic_hook() {
    console_error_panic_hook::set_once();
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn ensure_panic_hook() {}

// ---------------------------------------------------------------------------
// OOM diagnostic — prints to the JS console before the wasm trap fires.
//
// Without this, OOM goes through `handle_alloc_error → intrinsics::abort()`
// which bypasses the panic hook entirely, leaving the user with an opaque
// "RuntimeError: unreachable executed" and zero diagnostic info.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(msg: &str);
}

#[cfg(target_arch = "wasm32")]
#[alloc_error_handler]
fn alloc_error_handler(layout: std::alloc::Layout) -> ! {
    error(&format!(
        "WASM OOM: allocation of {} bytes (align {}) failed — \
         linear memory at ~4 GB hard cap. \
         The file is too large for browser conversion; use the CLI instead.",
        layout.size(),
        layout.align(),
    ));
    std::process::abort();
}

pub(crate) const DEFAULT_BASE_URI: &str = "https://lbd.example.com/";
