# Geometry Module Plan

## Status: CURRENT — supersedes all previous geometry plans

Previous plans that are now obsolete and should be ignored:
- `fragments-producer-handoff.md` — fragments approach being replaced by this
- `fragments-geometry-port-plan.md` — web-ifc C++ port abandoned, using ifc-lite instead

---

## What We Are Building

Two new plugins on top of ifc-lite's proven geometry engine:

1. **`plugin-geometry-preprocess`** (Preprocess stage) — tessellates all IFC geometry using ifc-lite math, stores `TessellatedModel` in `PipelineContext`. Runs once. All downstream consumers read from it.

2. **`plugin-geometry-producer`** (Producer stage) — reads `TessellatedModel`, serializes to chosen format (fragments / glTF / Parquet / IFC5), emits via `sidecar_tx`.

No new pipeline stage. Follows the same conventions as every other plugin in the codebase.

---

## ifc-lite Reuse Strategy

ifc-lite (`/tmp/web-ifc` or https://github.com/LTplus-AG/ifc-lite, license MPL-2.0) contains a pure Rust geometry crate at `rust/geometry/`. We copy it directly into our workspace as `crates/ifc-geometry` and keep the code as-is.

**We use our own `ifc-step` parser. We do not use ifc-lite's parser.** The ifc-lite geometry functions take raw numeric inputs (profile point arrays, depth values, direction vectors, etc.). We extract these from our `StepFile` and pass them directly to ifc-lite geometry functions.

### Files to copy from ifc-lite `rust/geometry/src/`

| Source | Destination | Contents |
|---|---|---|
| `tessellation.rs` | `crates/ifc-geometry/src/tessellation.rs` | ExtrudedAreaSolid, ExtrudedAreaSolidTapered, PolygonalFaceSet, TriangulatedFaceSet, FacetedBrep, SweptDiskSolid, RevolveSolid |
| `boolean.rs` | `crates/ifc-geometry/src/boolean.rs` | CSG boolean ops — BSP tree (csg.js port), half-space clipping |
| `curves.rs` | `crates/ifc-geometry/src/curves.rs` | All IFC profile types: Rectangle, Circle, CircleHollow, Arbitrary, I/T/L/Z/C/U profiles, CompositeProfile |
| `mesh.rs` | `crates/ifc-geometry/src/mesh.rs` | `Mesh { positions: Vec<f32>, normals: Vec<f32>, indices: Vec<u32> }` |
| `triangulate.rs` | `crates/ifc-geometry/src/triangulate.rs` | Polygon triangulation: earcutr for general case, fan for convex, direct for triangles/quads |
| `transform.rs` | `crates/ifc-geometry/src/transform.rs` | 4×4 matrix operations, decompose, compose |

### What we do NOT copy from ifc-lite

- Their IFC parser — we have `ifc-step`
- Their schema layer — we have `ifc-schema`
- Their WASM bindings — we have `ifc2lbd-wasm`
- Their TypeScript export packages — we implement output in `plugin-geometry-producer`
- Their clash detection — may add later as separate module

### Key dependency: earcutr

ifc-lite uses the `earcutr` crate for polygon triangulation (ear-clipping algorithm). This handles holes correctly. Add to `crates/ifc-geometry/Cargo.toml`:

```toml
[dependencies]
earcutr = "0.4"
nalgebra = "0.32"
```

Both compile to WASM without issues.

---

## New Crates

### `crates/ifc-geometry`

Pure math. No IFC entity knowledge. WASM-compatible. MPL-2.0 (add license header).

Public API:
```rust
pub fn extrude_area_solid(profile: &[f64], position: &[[f64;4];4], direction: [f64;3], depth: f64) -> Mesh
pub fn polygonal_face_set(coords: &[[f64;3]], faces: &[Vec<usize>]) -> Mesh
pub fn triangulated_face_set(coords: &[[f64;3]], index: &[[usize;3]]) -> Mesh
pub fn faceted_brep(shells: &[Vec<Vec<usize>>], points: &[[f64;3]]) -> Mesh
pub fn boolean_result(op: BooleanOp, first: &Mesh, second: &Mesh) -> Mesh
pub fn revolve_area_solid(profile: &[f64], axis: [f64;3], origin: [f64;3], angle: f64) -> Mesh
pub fn swept_disk_solid(directrix: &[[f64;3]], radius: f64) -> Mesh
```

### `crates/tessellated-model`

Shared data model only. No math. Depends on `ifc-geometry` for `Mesh`.

```rust
pub struct TessellatedModel {
    pub meshes: Vec<TessellatedMesh>,
    pub coordination_matrix: [[f64; 4]; 4],
    pub metadata_mode: MetadataMode,
}

pub struct TessellatedMesh {
    pub express_id: u64,
    pub guid: String,
    pub category: String,                   // entity name e.g. "IFCWALL"
    pub geometries: Vec<TessellatedGeometry>,
}

pub struct TessellatedGeometry {
    pub mesh: ifc_geometry::Mesh,
    pub color: [f32; 4],                    // RGBA 0-1, from IFCSTYLEDITEM chain
    pub world_transform: [[f64; 4]; 4],     // first geometry world matrix
    pub local_transform: [[f64; 4]; 4],     // firstGeom^-1 × thisGeom
    pub geometry_id: u64,                   // STEP entity ID, for dedup key
}

#[derive(Clone, Copy, Default)]
pub enum MetadataMode {
    #[default] Full,    // GUIDs, categories, attributes, relations
    Stripped,           // geometry + GUIDs only
}
```

### `crates/plugin-geometry-preprocess`

Preprocess plugin. Dispatches IFC geometry entities to `ifc-geometry` functions.

Module ID: `neo-geometry-preprocess`

Options:
- `metadata` = `full` | `stripped` (default: `full`)

### `crates/plugin-geometry-producer`

Producer plugin. Multiple output formats via Cargo features + module option.

Module ID: `neo-geometry-producer`

Options:
- `format` = `fragments` | `gltf` | `parquet` | `ifc5` (default: `fragments`)

---

## `plugin-geometry-preprocess` — Detail

### What it does

For each element in the element set (IFCWALL, IFCSLAB, IFCSPACE, etc. — excluding IFCOPENINGELEMENT, matching oracle's `classes.elements`):

1. Read `ObjectPlacement` → world transform via existing `product_world_transform`
2. Read `Representation` → iterate shape representation items
3. For each item, dispatch to `ifc-geometry` by entity type (see dispatch table below)
4. Extract color via `IFCSTYLEDITEM → IFCSURFACESTYLE → IFCSURFACESTYLERENDERING → IFCCOLOURRGB`
5. Compute local transform: `firstGeomWorldMatrix^-1 × thisGeomWorldMatrix`
6. Dedup geometry: by STEP entity ID first (`geometry_id`), then by content hash via `ifc-geometry::mesh_hash`
7. Build `TessellatedMesh`, accumulate into `TessellatedModel`
8. Store `Arc<TessellatedModel>` in `PipelineContext`

### Geometry dispatch

```rust
// Exact entity names from our StepFile, mapped to ifc-geometry functions
match entity.entity_name.as_str() {
    "IFCEXTRUDEDAREASOLID" => {
        let profile = extract_profile(step, entity.args[0]);
        let position = extract_axis2placement3d(step, entity.args[1]);
        let direction = extract_direction(step, entity.args[2]);
        let depth = extract_real(entity.args[3]);
        ifc_geometry::extrude_area_solid(&profile, &position, direction, depth)
    }
    "IFCEXTRUDEDAREASOLIDTAPERED" => ifc_geometry::extrude_area_solid_tapered(...),
    "IFCPOLYGONALFACESET" => {
        // arg[0]=Coordinates, arg[1]=Closed(skip), arg[2]=Faces
        let coords = extract_cartesian_point_list(step, entity.args[0]);
        let faces = extract_indexed_polygon_faces(step, entity.args[2]);
        ifc_geometry::polygonal_face_set(&coords, &faces)
    }
    "IFCTRIANGULATEDFACESET" => {
        // arg[0]=Coordinates, arg[1]=Normals(skip), arg[2]=Closed(skip), arg[3]=CoordIndex
        let coords = extract_cartesian_point_list(step, entity.args[0]);
        let index = extract_coord_index(entity.args[3]);
        ifc_geometry::triangulated_face_set(&coords, &index)
    }
    "IFCFACETEDBREP" => ifc_geometry::faceted_brep(...),
    "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => ifc_geometry::boolean_result(...),
    "IFCMAPPEDITEM" => recurse_with_accumulated_transform(...),
    "IFCREVOLVEDAREASOLID" => ifc_geometry::revolve_area_solid(...),
    "IFCSWEPTDISKSOLID" => ifc_geometry::swept_disk_solid(...),
    _ => None, // skip unsupported types
}
```

### Manifest

```rust
PluginManifest {
    id: "neo-geometry-preprocess",
    stage: PipelineStage::Preprocess,
    inputs: vec!["ifc-step", "ifc-model"],
    outputs: vec!["tessellated-model"],
    wasm_compatible: true,
    parallelism: ParallelismMode::Serial, // tessellation runs once
    failure_policy: FailurePolicy::Required,
    ...
}
```

---

## `plugin-geometry-producer` — Detail

### Cargo features for format gating

```toml
[features]
default = ["fmt-fragments"]
fmt-fragments = ["fragments-schema", "flate2"]
fmt-gltf = ["dep:gltf"]
fmt-parquet = ["dep:arrow2", "dep:parquet2"]
fmt-ifc5 = []               # only needs serde_json, already in workspace
```

WASM build: `default-features = true` (fragments only). CLI build: all features enabled. This keeps WASM bundle small.

### format=fragments (default)

Uses `fragments-schema` FlatBuffers. Replaces current `plugin-fragments-producer`.

1. Read `TessellatedModel` from context
2. Build `Meshes` FlatBuffer: shells from `ifc-geometry::getShellData` (the oracle profile-based format), samples, materials, transforms — same as oracle
3. If `metadata=full`: build entity/attribute/relation data from `StepFile` (same logic as current `convert.rs`)
4. Combine into `Model` FlatBuffer, zlib compress
5. Emit via `sidecar_tx` as `model.frag`

### format=gltf

Port of ifc-lite's glTF exporter.
1. Read `TessellatedModel`
2. Build glTF JSON + binary buffer (positions, normals, indices per mesh)
3. Package as `.glb`
4. Emit via `sidecar_tx` as `model.glb`

### format=parquet

Uses Apache Arrow Rust crate (same as ifc-lite — same performance).
1. Read `TessellatedModel`
2. Build Arrow record batches: one row per element, geometry as binary column
3. Write as Parquet with Snappy compression
4. Emit via `sidecar_tx` as `model.parquet`

### format=ifc5

IFC5/IFCX JSON format.
1. Read `TessellatedModel`
2. Serialize as IFCX (JSON) with USD-compatible geometry
3. Emit via `sidecar_tx` as `model.ifcx`

### Manifest

```rust
PluginManifest {
    id: "neo-geometry-producer",
    stage: PipelineStage::Produce,
    inputs: vec!["tessellated-model", "ifc-model", "ifc-step"],
    outputs: vec!["geometry-sidecar"],
    wasm_compatible: true,
    parallelism: ParallelismMode::ParallelByBatch,
    failure_policy: FailurePolicy::Required,
    ...
}
```

---

## Migration of Current Fragments Code

| Current | Replaced by |
|---|---|
| `crates/fragments-core/src/step.rs` | `crates/plugin-geometry-preprocess` + `crates/ifc-geometry` |
| `crates/fragments-core/src/convert.rs` | `crates/plugin-geometry-producer` (format=fragments) |
| `crates/fragments-core/src/shell_processor.rs` | `crates/ifc-geometry/src/tessellation.rs` (from ifc-lite) |
| `crates/plugin-fragments-producer` | deprecated, removed after new producer is stable |

Keep `plugin-fragments-producer` registered during migration for backward compat. Remove once `plugin-geometry-producer` passes parity tests.

---

## WASM/CLI Parity

Both new plugins registered in both:
- `crates/ifc2lbd-cli/src/pipeline_plugins.rs`
- `crates/ifc2lbd-wasm/src/plugins.rs`

WASM runner fix required (existing gap):
- `make_pipeline_context` in `runner.rs` must set `ctx.sidecar_tx`
- Drain sidecars after produce stage and send bytes to JS sink callback
- Same fix needed for fragments-producer today — do both at once

---

## Pipeline Validation Change

`validate_activation_plan_with_args` currently requires at least one LBD producer. Add `neo-geometry-producer` to the valid-producer check so geometry-only workflows are accepted.

---

## Sample Workflows

### Geometry-only (fragments, stripped — visualization)

```bash
ifc2lbd-neo model.ifc \
  --module neo-geometry-preprocess \
  --module-opt neo-geometry-preprocess.metadata=stripped \
  --module neo-geometry-producer \
  --module-opt neo-geometry-producer.format=fragments \
  --module neo-file-export
```

### Full LBD + fragments

```bash
ifc2lbd-neo model.ifc \
  --module neo-geometry-preprocess \
  --module neo-bot-producer \
  --module neo-geometry-producer \
  --module neo-turtle-serializer \
  --module neo-file-export
```

### glTF export

```bash
ifc2lbd-neo model.ifc \
  --module neo-geometry-preprocess \
  --module neo-geometry-producer \
  --module-opt neo-geometry-producer.format=gltf \
  --module neo-file-export
```

---

## Implementation Order

1. `crates/ifc-geometry` — copy ifc-lite geometry crate, strip non-geometry deps, adapt Cargo.toml
2. `crates/tessellated-model` — define shared data model
3. `crates/plugin-geometry-preprocess` — wire ifc-geometry to StepFile data
4. `crates/plugin-geometry-producer` (fragments format first) — migrate fragments-core logic, use ifc-geometry tessellation
5. WASM sidecar_tx fix
6. glTF, Parquet, IFC5 formats (sequential)

---

## Workspace Changes

```toml
# Cargo.toml workspace members
"crates/ifc-geometry",
"crates/tessellated-model",
"crates/plugin-geometry-preprocess",
"crates/plugin-geometry-producer",
```

---

## License Note

ifc-lite is MPL-2.0. Copied files must retain the MPL-2.0 header and stay under MPL-2.0. The rest of our codebase is unaffected — MPL-2.0 allows combining with other licenses in a larger work. Add a `LICENSE-ifc-geometry` file noting the origin.
