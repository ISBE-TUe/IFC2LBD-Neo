# Geometry Shell Dedup — Open Problem & Options

## Status

Shell dedup currently matches the **old Rust fragments-core baseline** (STEP-entity-ID dedup + oracle content hash on polygon geometry):

| Metric | Ours | Oracle Rust | Oracle TS (web-ifc) |
|--------|------|-------------|---------------------|
| shells | 1502 | 1502 | **662** |
| local_transforms | 595 | 595 | **1499** |
| samples | 3855 | 3855 | 3855 |

The gap between 1502 and 662 is **2.3×** more unique shell buffers sent to the GPU. For small models this is fine; for 163MB models like `cx-model-test` it produces 29MB fragments vs the ~14MB web-ifc would achieve. Under sustained load this will be noticeable in web viewers.

## Root cause

The **1502 Rust ceiling** comes from STEP-entity-level dedup: elements that share the same `IFCREPRESENTATIONMAP` get the same geometry IDs and dedup by ID. Elements with *direct* geometry (IFCEXTRUDEDAREASOLID directly in their shape rep, not via a RepMap) each have their **own STEP entity IDs** even when the geometry shape is identical (e.g. fifty identical wall cross-sections defined independently). The oracle content hash (on oracle's polygon mesh) catches some of these but not enough.

The **662 web-ifc ceiling** is higher because web-ifc operates at the tessellated-buffer level. It assigns a `geometryExpressID` to a *mesh buffer*, not a STEP entity. Identical geometry across structurally different STEP entities gets the same buffer ID. Our Rust STEP traversal can't replicate this without re-doing web-ifc's internal geometry dedup.

The **local_transforms gap** (595 vs 1499): same root cause — oracle's `flatTransformation` encodes the full element-placement × mapping × item-position chain including web-ifc-internal per-geometry variations we don't reproduce.

## Option A — Content hash on position-free ifc-lite mesh (recommended first step)

**How it works**: Instead of oracle's `hash_shell` (which hashes oracle's coarse polygon mesh), compute the content hash on the ifc-lite position-free tessellated mesh stored in `item_mesh_map`. Same extruded shape at different IFC positions → same position-free ifc-lite tessellation → same hash → dedup.

**What this changes in `plugin-geometry-producer/src/lib.rs`**:
- In `build_meshes`, on a hash-miss in `shell_dedup_by_id`, compute `hash_ifc_mesh(item_mesh_map[item_id])` instead of (or in addition to) `hash_shell(&instance.shell)`.
- `hash_ifc_mesh` must work on the **position-free** f32 mesh: compute area_sum, volume, centroid, and firstVertex in **f64** (promote f32 → f64 before accumulating) with `round = |v: f64| (v * 10_000.0).round() as i64`.

**Why this should be better than the current hash**: Oracle's polygon hash hashes *before* triangulation, so many triangles per polygon give different counts. Our triangulated mesh for the same solid always produces the same triangle count (ifc-lite is deterministic) → same hash across two STEP entities with the same shape. Position-free: identical extrusions at different positions hash identically.

**Expected improvement**: Should approach oracle TS's 662 for Revit-exported files (which define geometry inline per element without RepMaps). Possibly not reaching 662 exactly since ifc-lite triangulates differently from oracle's polygon mesh.

**Complexity**: Low. One function in `plugin-geometry-producer`. No changes to the geometry pipeline or vendored code.

**File**: `crates/plugin-geometry-producer/src/lib.rs` — `build_meshes`, replace `hash_shell` call with `hash_ifc_mesh` on the position-free ifc-lite mesh.

---

## Option B — Expose ifc-lite's internal geometry cache as the dedup key

**How it works**: ifc-lite already runs `get_or_cache_by_hash` internally for every geometry item. Two STEP entities that produce the same tessellation (identical profile, depth, orientation) share the same `Arc<Mesh>` pointer from the cache. Use the **Arc pointer address** (or the FxHash used internally) as the shell's `geometry_id` instead of the STEP entity ID.

**What this changes**:
1. In `vendor/geometry/geometry/src/mesh.rs`: expose a method on `GeometryRouter` that returns the cache hash for a given item (or expose the `Arc<Mesh>` pointer from the cache).
2. In `vendor/geometry/geometry/src/router/processing.rs`: `collect_submeshes_from_item_inner` sets `SubMesh.geometry_id` to the ifc-lite cache key rather than `item.id`.
3. In `crates/ifc-geometry/src/lib.rs`: surface the new geometry_id through `GeometryInstance.geometry_id`.
4. In `crates/plugin-geometry-producer/src/lib.rs`: `item_mesh_map` keyed by cache-based geometry_id instead of STEP ID.

**Consequence for local_transforms**: oracle's `geometry_instances_for_product` still uses STEP entity IDs for `item_id`. These no longer match ifc-lite's cache-based IDs. We'd need to either: (a) use ifc-lite's IDs for dedup and keep oracle's for transforms, or (b) recompute transforms from ifc-lite's cache IDs.

**Complexity**: Medium. Touches the vendored ifc-lite code, the geometry bridge crate, and the producer. No correctness risk — the cache already produces correct deduplicated geometry.

---

## Option C — Dual-hash dedup: STEP ID first, then ifc-lite content hash

**How it works**: Keep oracle's STEP ID for the first dedup pass. On a hash-miss, run `hash_ifc_mesh` on the position-free ifc-lite mesh. This is strictly better than the current oracle polygon hash (Option A) and doesn't require touching vendored code beyond what Option A does.

This is effectively **Option A** but renamed to emphasise it's an additive improvement on top of the oracle STEP-ID dedup that already works.

**Complexity**: Same as Option A.

---

## Option D — Reimplement web-ifc mesh-buffer-level dedup

**How it works**: After full ifc-lite tessellation of all elements, build a global map `content_hash(mesh) → shell_slot`. Two elements whose ifc-lite output hashes to the same value share a shell. This is the closest we can get to web-ifc's `geometryExpressID` system without using web-ifc.

**Requirements**:
- The hash must be robust: use Merkle-style hashing of sorted triangle edges or canonical vertex form.
- Must handle floating-point near-identity (round to ~0.01mm precision before hashing).
- The resulting geometry_id is still per-shell-slot, not per-STEP-entity.
- local_transforms would be recomputed from the shell-slot mapping.

**Complexity**: High. Correctness-critical: false positives (hash collision) would merge two geometrically different shells, corrupting the model.

---

## Option E — Run web-ifc geometry extraction (subprocess / WASM)

**How it works**: Spawn a Node.js subprocess (or load web-ifc WASM) to extract `geometryExpressID` and `flatTransformation` for every geometry instance, then map those IDs to ifc-lite's tessellated meshes.

**This is the only way to achieve exactly 662/1499** for sample.ifc. All Rust-only approaches are bounded by STEP-level dedup (1502 ceiling).

**Complexity**: Very high. Adds a Node.js/WASM runtime dependency to the CLI. Probably not worth it given the infrastructure cost.

---

## Recommended path

1. **Implement Option A/C** first — it's a small change with measurable impact. Measure the new shell count on the test suite. If it reaches ~700-900 (vs 1502 today), ship it.
2. **Evaluate Option B** if Option A leaves a meaningful gap after measurement. The ifc-lite cache already does this dedup internally; surfacing the key is mechanical work.
3. **Accept the residual gap** vs Option E. The gap between the Rust ceiling and web-ifc's 662 is intrinsic to STEP parsing; closing it requires web-ifc, which is a major infrastructure commitment.

## Impact table (estimated)

| Option | Expected shells | Complexity | Touches vendored? |
|--------|----------------|------------|-------------------|
| Current (baseline) | 1502 | — | no |
| A/C: ifc-lite content hash | ~700–900 | Low | no |
| B: ifc-lite cache key | ~650–750 | Medium | yes |
| D: global buffer dedup | ~650–700 | High | no |
| E: web-ifc subprocess | 662 (exact) | Very high | no |
