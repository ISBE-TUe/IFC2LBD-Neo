# Modularization Gaps Found During bSDD Integration

This note records architectural gaps discovered while integrating `neo-bsdd-producer`.
These are general modularization issues, not bSDD-specific.

## 1) WASM runner still has hardcoded producer booleans

### Problem
`ifc2lbd-wasm` still uses per-producer booleans (`emit_bot`, `emit_beo`, etc.) and hardcoded producer lists/branches in multiple execution paths.

### Why this hurts modularization
- Adding a new producer requires edits in many places.
- A module can be present in the activation plan but still not run if one hardcoded path omits it.
- This violates the intended "module-first" architecture where the resolved activation plan should drive execution.

### Where
- [`crates/ifc2lbd-wasm/src/types.rs`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/crates/ifc2lbd-wasm/src/types.rs)
- [`crates/ifc2lbd-wasm/src/validation.rs`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/crates/ifc2lbd-wasm/src/validation.rs)
- [`crates/ifc2lbd-wasm/src/runner.rs`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/crates/ifc2lbd-wasm/src/runner.rs)

### Recommended direction
- Replace per-module booleans with plan-driven checks (`ActivationPlan.enabled_ids`) at dispatch and drain points.
- Centralize producer receiver handling to avoid repeated manual lists in turtle/nquads/memory paths.

## 2) Web UI defaults/templates are hardcoded to specific producer IDs

### Problem
Web pipeline templates and ordering are hardcoded arrays of module IDs.

### Why this hurts modularization
- Newly registered modules do not appear in defaults/templates automatically.
- UI can diverge from backend registry contents.

### Where
- [`web/wasm-prototype/src/pipeline/app.js`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/web/wasm-prototype/src/pipeline/app.js)
- [`web/wasm-prototype/src/pipeline/session.js`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/web/wasm-prototype/src/pipeline/session.js)
- [`web/wasm-prototype/src/pipeline/state.js`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/web/wasm-prototype/src/pipeline/state.js)

### Recommended direction
- Derive template candidates/default stage ordering from `listModules()` + manifest metadata.
- Keep only UX ordering hints configurable, not fixed producer inventories.

## 3) WASM build script depended on rustup proxy semantics

### Problem
`scripts/build_wasm_web.sh` used `cargo +nightly ...`, which fails when `cargo` is a non-rustup binary (e.g. Homebrew cargo).

### Why this hurts modularization/reproducibility
- Build path becomes environment-specific.
- Team members can silently get stale WASM artifacts if script fails and they continue with old bundles.

### Where
- [`scripts/build_wasm_web.sh`](/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/ifc2lbd-neo/scripts/build_wasm_web.sh)

### Status
- Fixed in this branch by resolving nightly `cargo`/`rustc` via `rustup which --toolchain nightly ...`.

## 4) Packaging/source-of-truth mismatch risk (backend vs web bundle)

### Problem
Backend Rust changes can compile, but web runtime behavior still uses old WASM bundle unless rebuild script runs before web build.

### Why this hurts modularization
- Module registration in Rust does not guarantee module availability in browser runtime.
- Easy to mistake UI/backend mismatch for logic bugs.

### Recommended direction
- Enforce wasm artifact freshness in web build pipeline.
- Add CI check that fails if Rust sources changed but wasm bundle is stale.

