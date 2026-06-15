# Plan: Upgrade vendored ifc-lite to the pure-Rust exact-arithmetic CSG kernel

**Status:** Proposed · **Date:** 2026-06-12 · **Owner:** geometry

## Goal

Replace our current dual CSG stack — **C++ Manifold** on native/CLI and **legacy BSP** on
WASM — with the single pure-Rust exact-arithmetic kernel the ifc-lite author just shipped.
Outcome we want: **bit-identical geometry across x86_64 / aarch64 / wasm32**, watertight cuts
on coplanar/tilted openings, and removal of the C++ toolchain (`manifold-csg-sys`, `cc`,
`cmake`) from the native build.

## Where we are today

- Vendored, **not** a submodule — plain copies under [vendor/geometry/](../../vendor/geometry/):
  - `ifc-lite-core` **3.0.0** — [vendor/geometry/core/Cargo.toml](../../vendor/geometry/core/Cargo.toml)
  - `ifc-lite-geometry` **3.0.0** — [vendor/geometry/geometry/Cargo.toml](../../vendor/geometry/geometry/Cargo.toml)
  - Wired as path deps in the workspace ([Cargo.toml:107-109](../../Cargo.toml#L107-L109)), MPL-2.0.
- CSG kernel is feature-gated in [vendor/geometry/geometry/src/csg.rs](../../vendor/geometry/geometry/src/csg.rs):
  `subtract_mesh()` dispatches to `manifold_kernel::difference()` under `#[cfg(feature = "manifold-csg")]`,
  otherwise the `bsp_csg` port.
  - CLI enables it: [crates/ifc2lbd-cli/Cargo.toml:32](../../crates/ifc2lbd-cli/Cargo.toml#L32) (`features = ["manifold-csg"]`).
  - WASM does **not** → BSP fallback in the browser.
- 2D void subtraction uses `i_overlay` (`bool2d.rs`) and triangulation uses `earcutr` — these are
  already pure Rust and are **out of scope** unless upstream changed them.

## ⚠️ The one real risk: this is a *modified* vendored copy

The vendored tree carries local adaptations (e.g. the `#[cfg_attr(feature = "manifold-csg", allow(dead_code))]`
annotations, the WASM-safe wiring noted at [Cargo.toml:107](../../Cargo.toml#L107)). A naive
"copy the new upstream over vendor/" **will silently drop our patches**. Every step below is built
around establishing and preserving the delta against pristine upstream.

## Phase 0 — Confirm the upgrade is real and reachable

- [ ] Verify the new kernel is in a **published/tagged** ifc-lite release, not just a branch. Identify
      the exact version that contains it (call it `vX.Y.Z`).
- [ ] Confirm license is still MPL-2.0 and that our vendoring obligations (keep `vendor/geometry/LICENSE`)
      are unchanged.
- [ ] Read the upstream changelog/PR for the kernel: does it keep the same public API
      (`Csg::subtract_mesh`, `subtract_box`, the `Mesh` type, `bool2d`/`triangulation` modules)?
      Note any signature or module renames — these drive Phase 3.
- [ ] Confirm the new kernel removes the `manifold-csg` feature entirely (single path) vs. adds a new
      opt-in feature. This determines whether we delete the feature or just flip its default.

## Phase 1 — Establish the baseline (the safety net)

- [ ] Pin a pristine copy of our **current** upstream (3.0.0) somewhere out-of-tree and `diff -ru`
      it against `vendor/geometry/`. **Capture our local patch set** as a saved diff —
      this is the thing we must re-apply or consciously drop.
- [ ] Generate **golden geometry outputs** on the current stack, for both kernels, across the existing
      fixtures:
      `Duplex.ifc`, `Wohn-Geschaeftshaus.ifc`, `model-A.ifc`,
      `Building-Architecture(1).ifc`, `model-D.ifc`,
      `model E-STE-66-S1-REI-CN3-660100-0B.ifc`,
      `CX_AP2.0_..._Koordinationsmodell (1).ifc`.
  - For each: native CLI output (Manifold path) **and** WASM output (BSP path) → `.frag` + glTF.
  - Record a stable hash per mesh (we already quantize+hash for dedup in
    [crates/plugin-geometry-producer/src/lib.rs](../../crates/plugin-geometry-producer/src/lib.rs)),
    plus triangle counts and bounding boxes. Save as the comparison baseline.
- [ ] Snapshot the current `proof-bin` / `diagnostics` boolean-failure counts (`csg.rs` tracks
      `BoolFailure`s) so we can prove the new kernel reduces, not increases, failures.
- [ ] Do this all on a fresh branch: `feature/ifc-lite-exact-csg`.

## Phase 2 — Re-vendor upstream

- [ ] Drop the new upstream `vX.Y.Z` `core` + `geometry` crates into `vendor/geometry/`.
- [ ] Re-apply our saved local patch set from Phase 1. For each patch decide: **still needed?**
      (Many `manifold-csg` dead-code annotations become moot once the feature is gone.)
- [ ] Update vendored crate versions and refresh `vendor/geometry/LICENSE` if upstream's changed.
- [ ] **Record the exact upstream commit/tag** in a new `vendor/geometry/VENDORING.md` (we have no
      provenance file today — add one so the next upgrade isn't archaeology).

## Phase 3 — Rewire the build

- [ ] In [crates/ifc-geometry/Cargo.toml:9](../../crates/ifc-geometry/Cargo.toml#L9): remove/retire the
      `manifold-csg` feature passthrough (or repoint it at the new kernel feature if upstream kept one).
- [ ] In [crates/ifc2lbd-cli/Cargo.toml:32](../../crates/ifc2lbd-cli/Cargo.toml#L32): drop `manifold-csg`
      from the feature list.
- [ ] Delete now-dead code paths once green: `vendor/geometry/geometry/src/bsp_csg.rs`,
      `manifold_kernel.rs`, and the `#[cfg(feature = "manifold-csg")]` branches in `csg.rs` /
      `lib.rs` — **only if** upstream's new kernel subsumes them. Keep them until Phase 4 proves parity.
- [ ] Confirm `manifold-csg`, `manifold-csg-sys`, and (if no longer used) `cc` / `cmake` drop out of
      `Cargo.lock`.
- [ ] Verify [.cargo/config.toml](../../.cargo/config.toml) WASM flags still apply; the C++/LLVM
      cross-compile concerns the author mentioned were never our WASM path (we used BSP), but confirm
      no native build script now needs a different target setup.

## Phase 4 — Validate (the part that makes it "safe")

- [ ] `cargo build` + `cargo test` for the full workspace on native.
- [ ] Build the WASM crate and run the prototype ([web/wasm-prototype/](../../web/wasm-prototype/)).
- [ ] **Cross-platform determinism check** — the headline claim. Run the same fixtures on
      native (x86_64 and/or aarch64) **and** WASM; assert mesh hashes are now **identical**
      native↔WASM (today they differ — Manifold vs BSP). This is the acceptance test for the upgrade.
- [ ] **Regression check vs Phase 1 baseline** — diff new outputs against golden. Expect changes
      (different kernel), so triage each: opening cuts should be *equal or better* (watertight,
      fewer slivers). Use the `.frag` debug viewer ([web/fragments-debugviewer/](../../web/fragments-debugviewer/))
      to eyeball walls/slabs/openings on the worst offenders.
- [ ] Confirm `BoolFailure` count is ≤ baseline across all fixtures.
- [ ] **Perf check** — exact-arithmetic cascades (interval → fixed-width → BigRational) can be slower
      than Manifold on clean meshes. Time the CLI on the largest fixtures
      (`model D_...`, `model E-...`, `model A`). If regressed, note whether it's acceptable for the
      determinism/robustness win, or raise upstream.
- [ ] Note the deep-recursion stack workaround in `ifc-geometry` (1 GB stack thread for BSP) — the new
      kernel likely makes it unnecessary; remove only after WASM + native both pass.

## Phase 5 — Land

- [ ] Update the stale comment at [Cargo.toml:107](../../Cargo.toml#L107) ("no manifold-csg") to reflect
      the single-kernel reality.
- [ ] Update the WASM↔CLI sync note in project memory if the kernel split it described no longer exists.
- [ ] PR with: the determinism proof (native==WASM hashes), before/after failure counts, perf numbers,
      and the dropped C++ dependencies as the headline win.

## Rollback

All changes are on `feature/ifc-lite-exact-csg` and the kernel swap is the vendored tree + a few
`Cargo.toml` feature lines. If validation fails, revert the branch — the 3.0.0 vendored copy and the
`manifold-csg` feature wiring come back intact. Keep `bsp_csg.rs`/`manifold_kernel.rs` in-tree until
Phase 4 is fully green so rollback never requires re-vendoring.

## Open questions (resolve in Phase 0)

1. Is the new kernel in a tagged release yet, and at what version?
2. Does it keep the `Csg` public API, or do callers in `csg.rs`/`router/voids.rs` need changes?
3. Is it a drop-in (feature removed) or a new opt-in feature?
4. Does upstream still ship `bool2d` (i_overlay) + `earcutr` triangulation unchanged, or did the
   exact-arithmetic work absorb the 2D path too?
