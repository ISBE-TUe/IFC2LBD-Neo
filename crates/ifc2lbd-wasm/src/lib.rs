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

pub(crate) const DEFAULT_BASE_URI: &str = "https://lbd.example.com/";
