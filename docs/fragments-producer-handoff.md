# Fragments Producer Handoff

## Reference implementation

- `engine_fragment` (`ThatOpen/engine_fragment`) cloned to `/tmp/engine_fragment`
- NOT `worker.mjs` (that is the compiled bundle of the same TS source)
- Authoritative source files listed at bottom

## What Was Added

- `crates/fragments-schema` — FlatBuffers schema crate
- `crates/fragments-core` — native conversion + `frag-diff` diff tool (`src/bin/frag_diff.rs`)
- `crates/plugin-fragments-producer` — producer plugin wrapper
- `scripts/validate_fragments_parity.py` — end-to-end parity harness
- `crates/ifc2lbd-cli/benches/conversion.rs` — benchmark

## Oracle Algorithm (from TS source)

### `property-processor.ts` — Entity/attribute/relation serialization

**Three passes:**

1. Elements that had geometry (geometry processor pass)
2. All IDs from `classes.abstract ∪ classes.elements` per type from `classes.ts`:
   - `base`: IFCPROJECT, IFCSITE, IFCBUILDING, IFCBUILDINGSTOREY
   - `units`: IFCUNITASSIGNMENT, IFCSIUNIT, IFCNAMEDUNIT, IFCDERIVEDUNIT, IFCMONETARYUNIT
   - `materials`: IFCMATERIAL*, IFCMATERIALLAYER*, IFCMATERIALPROFILE*, IFCMATERIALCONSTITUENT*
   - `properties`: IFCPROPERTYSET, IFCPROPERTYSINGLEVALUE, IFCELEMENTQUANTITY, IFCQUANTITY*
   - `types`: all `*TYPE` entities
   - `elements`: all physical element types (IFCSPACE included, IFCOPENINGELEMENT excluded)
3. Relation pass — 5 IFCREL* types only — NOT added to entity list, only populate `_relationsMap`:
   - IFCRELAGGREGATES → `IsDecomposedBy` / `Decomposes`
   - IFCRELDEFINESBYPROPERTIES → `DefinesOccurrence` / `IsDefinedBy`
   - IFCRELDEFINESBYTYPE → `ObjectTypeOf` / `IsTypedBy`
   - IFCRELASSOCIATESMATERIAL → `AssociatedTo` / `HasAssociations`
   - IFCRELCONTAINEDINSPATIALSTRUCTURE → `ContainsElements` / `ContainedInStructure`

**Per-entity attribute encoding:**
- Skip: `null`, `undefined`, `number`, `boolean`, `attributesToExclude` = {Representation, ObjectPlacement, CompositionType, OwnerHistory}
- Array refs (type=5) → relations. Non-refs → `[attrName, [v1,...], typeName]`
- Scalar ref → relation `[attrName, id]`
- Everything else → `[attrName, value, typeName]`
- `GlobalId` → GUID only, not stored as attribute

**GUIDs:** only from IfcRoot entities — GlobalId is exactly 22-char base64-like string (`[0-9A-Za-z_$]`).

### `ifc-file-reader.ts` + `geometry/index.ts` — Geometry serialization

**Element iteration:** `StreamMeshes` per type in `classes.elements`. Each `FlatMesh` has per-geometry instances with `flatTransformation` (world matrix) and `color`.

**Geometry stays in LOCAL/DEFINITION SPACE** — transforms are never baked into vertex positions.

**`flatTransformation` = `elementPlacement × itemTransforms`** — the full world-space matrix.

**Shell deduplication:** metric-based hash = `vertexCount-triangleCount-areaSum-biggestArea-volume-cx-cy-cz-x1-y1-z1` (all floats rounded to 4 decimal places).

**Local transform computation (`getLocalTransform`):**
```
localTransform[i] = firstGeomTransform^(-1) × geomTransform[i]
```
Where `firstGeomTransform` = first geometry's world matrix = `elementPlacement × firstItemTransform`.
First geometry per element → `localTransform = null` → sample stores 0 (identity at `local_transforms[0]`).
Identity check: `px=0, py=0, pz=0, dxx=1, dxy=0, dxz=0, dyx=0, dyy=1, dyz=0`.

**Local transforms vector:** identity is explicitly stored first (`id: 0, data: [0,0,0,1,0,0,0,1,0]`).
`sample.local_transform = K` → access `local_transforms[K]` directly (0-indexed).

**Colors:** per geometry from `geometryRef.color` (RGBA 0–1).

**Global transform** = first geometry's world matrix = `elementPlacement × firstItemTransform`.

## Current Parity Status (DigitalHub.ifc)

| Field | Oracle | Native | Note |
|---|---|---|---|
| local_ids | 24,446 | 24,444 | -2 minor |
| guids | 9,837 | **9,837** | exact ✓ |
| relations | 10,936 | 10,925 | -11 minor |
| mesh_items | 769 | 520 | -249 unsupported geom types |
| samples | 3,855 | 1,433 | follows from mesh_items |
| shells | 662 | 301 | oracle's 662 = our 301 + 361 from missing elems |
| materials | 26 | 15 | partial IFCSTYLEDITEM traversal |
| local_transforms | 1,499 | 595 | oracle's 1499 = our 595 + 904 from missing elems |
| raw bytes | 3,527,224 | 5,282,688 | +50% — attr strings not deduped |

**Key insight about shells/local_transforms gap:** Our 301 shells and 595 local_transforms come from the 520 elements we DO process. The oracle's extra 361 shells and 904 local_transforms come from the 249 missing elements (those with unsupported geometry types). The algorithm is correct — the gap is purely from missing geometry type support.

## Root cause of geometry gap (mesh_items 520 vs 769)

The oracle uses web-ifc's `StreamMeshes` which handles ALL IFC geometry types.
Our native handles: IFCEXTRUDEDAREASOLID, IFCBOUNDINGBOX, IFCTRIANGULATEDFACESET,
IFCPOLYGONALFACESET, IFCFACETEDBREP, IFCFACEBASEDSURFACEMODEL, IFCMAPPEDITEM (traversal),
IFCBOOLEANRESULT (fallback to operand 1).

Missing: IFCREVOLVEDAREASOLID, IFCSWEPTDISKSOLID, IFCSURFACECURVESWEPTAREASOLID,
complex NURBS/BREPs, and any geometry type not listed above.
249 elements in DigitalHub.ifc use these types and produce no geometry in the native.

## Implementation Notes

### Geometry coordinate space

Geometry is kept in LOCAL/DEFINITION SPACE (no transforms applied to vertices).
The item's own axis2placement3d is returned as a separate transform in `GeometryInstance.local_transform`.
Accumulated from: `parent_transform × mapping_target × mapping_origin × item_axis2placement3d`.

### Local transform computation

In `build_meshes`, for each element:
```
first_item_inv = instances[0].local_transform.inverse()
global_transform = product_world_transform × instances[0].local_transform

for i > 0:
    relative = first_item_inv × instances[i].local_transform
    if not identity: store as local_transform entry (1-indexed)
```

### Shell dedup hash

Metric-based hash matching oracle's `loadShellGeometry` hash string.
Based on: vertex count, triangle count, area sum, biggest area, volume, centroid,
first vertex. All floats rounded to `× 10000`.

## Remaining Work

### Priority 1: More geometry type support

Implement extractors for:
- `IFCREVOLVEDAREASOLID` — revolve profile around axis
- `IFCSWEPTDISKSOLID` — sweep disk along directrix (currently has CircleExtrusion path in oracle)
- `IFCSURFACECURVESWEPTAREASOLID` — general swept area
- Improve `IFCBOOLEANRESULT` — try both operands, not just first

Each needs: STEP → tessellated triangles in geometry's own local space (no axis2placement3d applied).

### Priority 2: Materials: improve IFCSTYLEDITEM traversal

Currently finds 15 of 26 materials. Check:
- IFCPRESENTATIONLAYERASSIGNMENT path (oracle may use it)
- DigitalHub uses IFCINDEXEDCOLOURMAP (1,426 entities) for per-face vertex colors — may be the source of the missing 11

### Priority 3: Fix -2 entity / -11 relation gap

Likely IFCCONVERSIONBASEDUNIT (1 entity). Extend `frag-diff` to list IDs in oracle but not native.

### Priority 4: Raw byte size

Oracle uses `createSharedString` in FlatBuffers builder for attribute strings.
Switching to the Rust FlatBuffers equivalent would deduplicate common attribute strings
(e.g., `["Name","...", "IFCLABEL"]`) across all entities, significantly reducing raw bytes.

### Priority 5: Full/Light mode

Add to `FragmentsConfig`:
```rust
pub enum FragmentsMode {
    Full,   // all entity data + geometry (default)
    Light,  // geometry only + one GUID per element
}
```

## Bugs Fixed

1. QuadChunkWriter tests: PathBuf → SharedSession API
2. Producer count test: 6 → 7
3. `has_any_producer` missing FRAGMENTS_PRODUCER_ID
4. Dead test referencing removed test.ifc → DigitalHub.ifc
5. IFCREL* entities in entity list → relations-only
6. GUID extraction: any 22-char string → now gated by `entity_type_is_ifc_root` + is_ifc_guid
7. IFCRELDEFINESBYTYPE matched ends_with("TYPE") → excluded IFCREL* from type check
8. IFCOPENINGELEMENT in entity list and geometry list → oracle excludes both
9. Entity #1 unconditionally pushed (was IFCORGANIZATION)
10. Relations used raw attr names → now 5 semantic IFCREL* types with oracle's mapped names
11. IFCMATERIALCONSTITUENTSET names falsely matched GlobalId
12. IFCSPACE missing from geometry processing
13. Local transforms vector missing identity at index 0
14. `geometry_instances_for_product` returned first IFCMAPPEDITEM child only → now collects all
15. Axis2placement3d was baked into extruded solid geometry → now returned as separate transform
16. Local transforms computed from mapping transform only → now uses `firstItemTransform^-1 × thisItemTransform`
17. Shell dedup used raw byte hash → now uses oracle's metric-based hash (vertex count, area, volume, centroid)
18. Global transform did not include first item's placement → now `elementPlacement × firstItemTransform`

## Useful Commands

```bash
# Build release
cargo build --release -p ifc2lbd-cli

# Run on DigitalHub
target/release/ifc2lbd-neo web/wasm-prototype/public/DigitalHub.ifc \
  -o /tmp/digitalhub.ttl \
  --module neo-bot-producer \
  --module fragments-producer \
  --module neo-turtle-serializer \
  --module neo-file-export

# Generate oracle output (clones engine_fragment if needed)
python3 scripts/validate_fragments_parity.py web/wasm-prototype/public/DigitalHub.ifc

# Structural diff
target/release/frag-diff /tmp/oracle.frag /tmp/model.frag

# Unit test
cargo test -p fragments-core --lib
```

## WASM Status

`FragmentsProducerPlugin` registered in `browser_registry()` but not wired up:
1. `active_producer_ids_from_settings` in `runner.rs` excludes FRAGMENTS_PRODUCER_ID
2. `make_pipeline_context` does not set `ctx.sidecar_tx`

Fix both before enabling in WASM.

## Upstream Reference

```
/tmp/engine_fragment/packages/fragments/src/Importers/IfcImporter/index.ts
/tmp/engine_fragment/packages/fragments/src/Importers/IfcImporter/src/classes.ts
/tmp/engine_fragment/packages/fragments/src/Importers/IfcImporter/src/geometry/index.ts
/tmp/engine_fragment/packages/fragments/src/Importers/IfcImporter/src/geometry/ifc-file-reader.ts
/tmp/engine_fragment/packages/fragments/src/Importers/IfcImporter/src/properties/property-processor.ts
```
