# QTO Preprocess Plugin — Implementation Plan

Status: planned  
Branch: `feature/qto`  
Plugin ID: `qto-preprocess`

---

## Goal

Add a preprocess plugin that detects missing IFC quantity sets and individual
quantities on building elements, computes them from STEP geometry, and injects
the results back into the `IfcModel` before any producer runs. Producers
(OPM, IfcOWL) then emit computed quantities exactly like native ones — no
producer changes are needed.

---

## Background and motivation

IFC files exported from authoring tools frequently omit quantity sets entirely
or include only partial data (e.g., `Length` but not `NetVolume`). LBD output
for QTO-heavy use cases (cost estimating, carbon accounting, scheduling) is
then incomplete. The geometry to compute these quantities is always present
in the STEP file — it just hasn't been extracted.

This plugin closes that gap without touching any downstream consumer. It is
opt-in, marked `FailurePolicy::Optional` (a file with no parseable geometry
skips silently), and never overwrites values already present in the source file.

---

## IFC standard quantity set names

IFC4/4.3 defines per-type `Qto_*BaseQuantities` sets. The plugin must use
these names exactly so that:
- bSDD matching resolves the set correctly (it matches by `ElementQuantity.name`)
- Downstream consumers expecting standard names are not confused
- A second set with the same name is never created

| IFC entity type | Quantity set name | Key quantities |
|---|---|---|
| `IfcWall`, `IfcWallStandardCase` | `Qto_WallBaseQuantities` | Length, Height, Width, GrossFootprintArea, NetSideArea, GrossVolume, NetVolume |
| `IfcSlab` | `Qto_SlabBaseQuantities` | GrossArea, NetArea, Width (thickness), GrossVolume, NetVolume |
| `IfcBeam` | `Qto_BeamBaseQuantities` | Length, CrossSectionArea, OuterSurfaceArea, GrossVolume, NetVolume |
| `IfcColumn` | `Qto_ColumnBaseQuantities` | Length, CrossSectionArea, OuterSurfaceArea, GrossVolume, NetVolume |
| `IfcDoor` | `Qto_DoorBaseQuantities` | Width, Height, Perimeter, Area |
| `IfcWindow` | `Qto_WindowBaseQuantities` | Width, Height, Perimeter, Area |
| `IfcSpace` | `Qto_SpaceBaseQuantities` | GrossFloorArea, NetFloorArea, Height, GrossVolume, NetVolume |
| `IfcStair` | `Qto_StairBaseQuantities` | Length, GrossVolume, NetVolume |
| `IfcRoof` | `Qto_RoofBaseQuantities` | GrossArea, NetArea |
| `IfcCovering` | `Qto_CoveringBaseQuantities` | GrossArea, NetArea |
| `IfcFooting`, `IfcPile` | `Qto_FootingBaseQuantities` / `Qto_PileBaseQuantities` | Length, GrossVolume |
| All other elements | `Qto_ElementBaseQuantities` | GrossVolume (BBox fallback only) |

The lookup table lives in `src/qto_names.rs` as a `const` array of
`(&str /* entity_name */, &str /* qto_name */, &[QuantityKind])`.

---

## Architecture

### Crate

```
crates/plugin-qto-preprocess/
├── Cargo.toml
└── src/
    ├── lib.rs          # QtoPreprocessPlugin struct, manifest, PreprocessPlugin impl, logging
    ├── qto_names.rs    # entity_name → (Qto_* name, expected quantity kinds) table
    ├── audit.rs        # scan IfcModel → per-element MissingQuantityReport
    ├── step_geom.rs    # STEP geometry traversal helpers (shared by all tiers)
    ├── bbox.rs         # Tier 1: AABB from CartesianPoint vertices
    ├── rep_parser.rs   # Tier 2: ExtrudedAreaSolid + ProfileDef extraction
    ├── mesh_volume.rs  # Tier 3: parry3d TriMesh volume/surface area
    └── inject.rs       # extend-or-create ElementQuantity + PhysicalQuantity in model clone
```

### Plugin manifest

```rust
PluginManifest {
    id: "qto-preprocess",
    display_name: "QTO preprocess",
    stage: PipelineStage::Preprocess,
    description: "Detects missing IFC quantity sets, computes them from STEP geometry, \
                  and injects results into the model before producers run.",
    inputs:  vec!["ifc-model", "ifc-step"],
    outputs: vec!["ifc-model"],
    requires: vec!["cleanup-preprocess"],   // run after dedup + normalisation
    conflicts_with: vec![],
    failure_policy: FailurePolicy::Optional,
    parallelism: ParallelismMode::Serial,
    wasm_compatible: true,                  // all three tiers are pure Rust
    named_graph_slug: None,
    needs_full_graph: false,
}
```

### Activation

Follows the same pattern as `BsddMatchPreprocessPlugin` — opt-in, not
registered by default. Enabled by a CLI flag / WASM option (details in the
registration section below).

---

## Processing pipeline (inside `preprocess()`)

```
StepFile + IfcModel
      │
      ▼
 1. audit()           → Vec<MissingQuantityReport>   (audit.rs)
      │
      ▼
 2. for each report:
      ├─ Tier 1: bbox::compute()         → BBox dimensions (always runs)
      ├─ Tier 2: rep_parser::compute()   → exact lengths/areas from STEP rep
      └─ Tier 3: mesh_volume::compute()  → NetVolume / SurfaceArea (if enabled)
      │
      ▼
 3. inject()          → cloned IfcModel with synthetic quantities added
      │
      ▼
 4. ctx.replace(Arc::new(augmented_model))
 5. log stats → PipelineLogBundle
```

---

## Module details

### `audit.rs`

Iterates `model.elements` (and `model.spatial_nodes` for `IfcSpace`).

For each element:
1. Look up its `Qto_*` name from `qto_names.rs`.
2. Collect all `ElementQuantity` records already linked via
   `model.quantities_for_object[element_id]`.
3. From those, collect the set of quantity names already present
   (`model.physical_quantities[qty_id].name`).
4. Subtract from the expected set for this type.
5. If any are missing, push a `MissingQuantityReport`.

```rust
pub struct MissingQuantityReport {
    pub element_id: EntityId,
    pub entity_type: SmolStr,
    pub qto_set_name: &'static str,
    pub existing_set_id: Option<EntityId>,   // Some → extend, None → create new
    pub missing: Vec<QuantityKind>,
}

pub enum QuantityKind {
    Length, Height, Width, Depth,
    GrossVolume, NetVolume,
    GrossArea, NetArea,
    GrossFootprintArea,
    CrossSectionArea, OuterSurfaceArea,
    Perimeter,
}
```

### `step_geom.rs`

Shared helpers for walking the STEP entity graph. Provides:
- `product_representation(step, element_id) → Option<EntityId>`  
  Walks `IFCPRODUCTREPRESENTATION` → `IFCSHAPEREPRESENTATION`.
- `first_solid(step, shape_rep_id) → Option<SolidKind>`  
  Returns the first solid found: `ExtrudedAreaSolid`, `FacetedBrep`, etc.
- `cartesian_points_of(step, entity_id) → Vec<[f64;3]>`  
  Recursively collects `IFCCARTESIANPOINT` references under any entity.

```rust
pub enum SolidKind {
    ExtrudedAreaSolid { profile_id: EntityId, depth: f64 },
    FacetedBrep { shell_id: EntityId },
    TriangulatedFaceSet { coord_ids: Vec<EntityId>, index_ids: Vec<EntityId> },
    Unknown,
}
```

### `bbox.rs` (Tier 1)

Given a list of `[f64;3]` points (from `step_geom::cartesian_points_of`),
compute AABB extents. Returns `Option<BBoxResult>`.

```rust
pub struct BBoxResult {
    pub width: f64,   // X extent
    pub depth: f64,   // Y extent
    pub height: f64,  // Z extent
}
```

Used as fallback for any element type and as the primary source for
`GrossVolume = width × depth × height` and `GrossFootprintArea = width × depth`.

### `rep_parser.rs` (Tier 2)

Given a `SolidKind::ExtrudedAreaSolid { profile_id, depth }`:

1. Read the profile entity (`IFCRECTANGLEPROFILEDEF`, `IFCCIRCLEPROFILEDEF`,
   `IFCARBITRARYCLOSEDPROFILEDEF`, …).
2. Compute profile area:
   - Rectangle: `x_dim × y_dim`
   - Circle: `π × r²`
   - ArbitraryClosedProfile: shoelace formula on `IFCPOLYLOOP` points
3. Return `RepResult { extrusion_depth: f64, profile_area: f64, profile_perimeter: f64 }`.

From this, derivable quantities:
- `Length` or `Height` = `extrusion_depth` (axis-dependent, see type table)
- `CrossSectionArea` = `profile_area`
- `NetVolume` = `extrusion_depth × profile_area`
- `OuterSurfaceArea` = `profile_perimeter × extrusion_depth`

### `mesh_volume.rs` (Tier 3)

Given a `SolidKind::FacetedBrep` or `TriangulatedFaceSet`, collect triangle
vertices from STEP face loops and build a `parry3d::shape::TriMesh`.

Compute:
- **NetVolume**: signed tetrahedral decomposition  
  `V = (1/6) · Σ vᵢ · (vⱼ × vₖ)` over all triangles, take `abs`.
- **NetSurfaceArea**: sum of triangle areas.

`parry3d` is pure Rust and compiles to WASM — no feature gate needed.
Enabled at runtime via `QtoOptions::compute_mesh_volume: bool` (default `false`
since tessellating complex BReps from raw STEP is slow on large files).

### `inject.rs`

Takes the audited model and computed values; returns a cloned `IfcModel` with
synthetic records inserted.

**Synthetic EntityId allocation:**  
Scan `step.entities.keys().max()` → `max_id`. Allocate synthetic IDs from
`max_id + 1` upward via a simple counter. These never appear in the STEP file
so no collision is possible.

**Extend-or-create logic:**
```
if report.existing_set_id is Some(set_id):
    clone the ElementQuantity at set_id
    append new PhysicalQuantity IDs to its .quantities vec
    replace entry in model.element_quantities
else:
    allocate new ElementQuantity with:
        id:                  next_synthetic_id()
        guid:                new UUIDv4 (smol_str)
        name:                Some(report.qto_set_name)
        method_of_measurement: Some("Computed")
        quantities:          vec of new PhysicalQuantity IDs
    insert into model.element_quantities
    push set_id into model.quantities_for_object[element_id]
```

For each `PhysicalQuantity`:
```rust
PhysicalQuantity {
    id:          next_synthetic_id(),
    entity_name: quantity_kind.ifc_entity_name(),  // e.g. "IfcQuantityVolume"
    name:        quantity_kind.ifc_name(),          // e.g. "NetVolume"
    value:       Some(StepValue::Real(computed_value)),
}
```

**Unit handling:**  
Before injecting, resolve the project unit for this quantity type from
`model.unit_assignment` + `model.units`. This entity ID is stored on
`PhysicalQuantity` (it already has a `value` field; unit resolution for
emission is done by the OPM producer via `resolve_quantity_unit`, which maps
`entity_name` to the project unit — no change needed there). Computed values
must be expressed in the project's own unit (e.g., mm³ if the file uses mm).
The `step_geom` coordinate extractor preserves raw STEP coordinate values;
if the file's length unit is mm, all distances are already in mm — no
conversion required. If the project unit is non-SI the conversion factor is
available via `Unit::ConversionBased.conversion_factor`.

---

## Logging

Follows the `PipelineLogBundle` / `write_module` pattern used by
`CleanupPreprocessPlugin` and `BsddMatchPreprocessPlugin`.

```rust
logs.write_module("qto-preprocess", json!({
    "elements_scanned":           total_elements,
    "elements_with_all_qto":      already_complete,
    "elements_missing_qto":       had_missing,
    "qto_sets_found_existing":    sets_extended,
    "qto_sets_created_new":       sets_created,
    "quantities_computed_total":  quantities_added,
    "quantities_by_kind": {
        "NetVolume":         n,
        "GrossVolume":       n,
        "Length":            n,
        "Height":            n,
        "Width":             n,
        "GrossArea":         n,
        "NetArea":           n,
        "CrossSectionArea":  n,
        "OuterSurfaceArea":  n,
        "GrossFootprintArea": n,
    },
    "tier_used": {
        "bbox_only":     n,   // element had no parseable rep solid
        "rep_parser":    n,   // ExtrudedAreaSolid found
        "mesh_volume":   n,   // FacetedBrep / TriFaceSet used (Tier 3)
    },
    "elements_skipped_no_geometry": skipped,
}));
```

This data appears in the export plugin's JSON stats output like all other
module logs — no changes to the export plugin are needed.

---

## Options

```rust
pub struct QtoOptions {
    /// Enable the plugin at all (default: false — opt-in).
    pub enabled: bool,
    /// Also run Tier 3 mesh volume (parry3d TriMesh). Slower on large files.
    pub compute_mesh_volume: bool,
    /// Overwrite quantities already present in the source file.
    /// Default false — only fills gaps.
    pub overwrite_existing: bool,
}
```

Exposed on the CLI and WASM `ConvertOptions` the same way other module options
are (env var `IFC2LBD_QTO_ENABLED=1`, WASM option key `"qto"`).

---

## Registration

### CLI — `crates/ifc2lbd-cli/src/pipeline_plugins.rs`

```rust
if options.qto_enabled {
    registry.register_preprocess(Box::new(QtoPreprocessPlugin::new(options.qto_options.clone())));
}
```

### WASM — `crates/ifc2lbd-wasm/src/plugins.rs`

Same conditional, driven by `ConvertOptions.qto`.

### `Cargo.toml` workspace

```toml
plugin-qto-preprocess = { path = "crates/plugin-qto-preprocess" }
```

Add to both `ifc2lbd-cli/Cargo.toml` and `ifc2lbd-wasm/Cargo.toml`.

---

## What this does NOT change

- `ifc-model`: no new types, no parser changes — synthetic records use the
  existing `PhysicalQuantity` and `ElementQuantity` structs.
- `lbd-converter` / OPM producer: no changes — computed quantities pass
  through the existing emission path transparently.
- `lbd-geometry` / `lbd-geometry-kernel`: not a dependency — geometry
  extraction happens directly from `StepFile` inside this plugin.
- bSDD matching: standard `Qto_*` names are already in the bSDD index, so
  computed sets get matched automatically if bSDD is enabled.

---

## Implementation order

1. `Cargo.toml` workspace + crate skeleton, `QtoOptions`, plugin manifest, stub `preprocess()`
2. `qto_names.rs` — full type → set name + quantity kind table
3. `audit.rs` — `MissingQuantityReport` generation
4. `step_geom.rs` — STEP traversal helpers
5. `bbox.rs` — Tier 1 BBox computation
6. `inject.rs` — extend-or-create logic, synthetic ID allocation, unit handling
7. `rep_parser.rs` — Tier 2 ExtrudedAreaSolid + profile area
8. `mesh_volume.rs` — Tier 3 parry3d TriMesh
9. Logging (`PipelineLogBundle` wiring)
10. CLI + WASM registration
11. Integration test: round-trip a minimal STEP fixture with known missing quantities, assert injected values
