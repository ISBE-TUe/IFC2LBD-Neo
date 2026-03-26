# WebAssembly Plan (Reviewed)

This document turns the initial idea in `docs/archive/webassembly-brainstorm.md` into a project-specific plan.

## Goal

Run basic IFC -> LBD conversion locally in the browser without uploading IFC files.

## Scope

Phase 1 scope (recommended):

- LBD-only conversion in browser.
- No IfcOWL sidecar in browser path (too heavy for first release).
- No exact-kernel geometry path in browser.
- Single-threaded Wasm first.

Out of scope for phase 1:

- OCC/chijin-based exact geometry.
- Full parity with server/CLI for all heavy models.
- Multi-threaded Wasm deployment.

## Why This Scope

- Keeps first browser build feasible and stable.
- Avoids native dependency blockers for `wasm32-unknown-unknown`.
- Keeps memory footprint manageable on typical client machines.

## Architecture

1. Extract/keep pure conversion core in Rust crates with no OS/file assumptions.
2. Add wasm facade crate exposing minimal API.
3. Browser UI handles file input and download output.
4. Optional Web Worker to prevent UI freezing.

## API Shape

Recommended wasm API:

```rust
#[wasm_bindgen]
pub fn convert_ifc_to_lbd(input: &[u8], base_uri: String) -> Result<Vec<u8>, JsValue>
```

Design notes:

- `input` is IFC bytes from browser File API.
- Return serialized TTL bytes.
- Keep options minimal for phase 1.

## Compatibility Risks in This Repo

- `ifc-step` currently uses `memmap2`; mmap is not a browser primitive.
- Some current optimizations assume native threading/runtime behavior.
- Large IFC files can exceed browser memory budgets.

Mitigation:

- Add a wasm-safe parse path from byte slice (no mmap).
- Keep browser path single-threaded initially.
- Add clear max-file guidance in UI.

## Delivery Plan

1. Build wasm-safe conversion path (no geometry, no IfcOWL).
2. Add `crates/ifc2lbd-wasm` with `wasm-bindgen` exports.
3. Add minimal web demo app (`web/`) with file input and output download.
4. Validate with small/medium fixtures.
5. Benchmark browser runtime + memory.

## Validation Criteria

- Output semantics for LBD basic mode match CLI basic mode (normalized compare).
- No file upload performed by app code.
- Conversion succeeds on representative small and medium fixtures.
- UI remains responsive (worker mode preferred).

## Security and Privacy Statement

Allowed claim:

- "Runs locally in your browser; no IFC upload required."

Condition:

- Frontend must not send IFC bytes via network APIs.

## Recommendation

Proceed with phase 1 exactly as above.

This is realistic and useful, but only if scoped tightly to browser-safe conversion first.
