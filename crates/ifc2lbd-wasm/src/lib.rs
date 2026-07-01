#![allow(unused_imports)]

use wasm_bindgen::prelude::wasm_bindgen;

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

/// Install a panic hook so that panics in WASM print the real message to the
/// JS console instead of the opaque `unreachable executed` trap string.
///
/// `console_error_panic_hook` only works on `target_arch = "wasm32"`.  On
/// wasm64 it's a no-op, so we use a custom hook that calls `console.error`
/// directly via `wasm_bindgen`.
///
/// Idempotent via `Once` — safe to call from every `#[wasm_bindgen]` entry
/// point.
#[cfg(target_family = "wasm")]
pub(crate) fn ensure_panic_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        // Try the external crate first (works on wasm32).
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();

        // On wasm64, console_error_panic_hook is a no-op, so install
        // our own hook that calls console.error directly.
        #[cfg(target_arch = "wasm64")]
        {
            #[wasm_bindgen]
            extern "C" {
                #[wasm_bindgen(js_namespace = console)]
                fn error(msg: &str);
            }
            std::panic::set_hook(Box::new(|info| {
                error(&info.to_string());
            }));
        }
    });
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn ensure_panic_hook() {}

pub(crate) const DEFAULT_BASE_URI: &str = "https://lbd.example.com/";
