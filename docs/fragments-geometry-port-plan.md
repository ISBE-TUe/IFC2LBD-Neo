# fragments-geometry Port Plan

Port of web-ifc's C++ geometry engine to Rust.
Source: `/tmp/web-ifc/src/cpp/web-ifc/geometry/`

## Goals

- Exact parity with web-ifc tessellation output — no guessing, port from source
- Works in CLI binary AND WASM target — no Node.js, no subprocess, no external runtime
- Reusable foundation for ALL geometry-dependent converter modules (not just fragments)

## New Crate: `crates/fragments-geometry`

Standalone Rust crate. No IFC-model dependency — operates directly on `StepFile`.
WASM-compatible: no `std::process`, no file I/O, pure computation.

Other modules consume it via:

```rust
use fragments_geometry::{FlatMesh, IfcGeometry, get_flat_mesh, stream_meshes};
```

## Reusability

`fragments-geometry` is not just for fragments. Any future module that needs real geometry can use it:

| Future module | Uses |
|---|---|
| fragments-producer | Shell tessellation, world transforms, colors |
| Topology / clash detection | Tessellated meshes for BVH / adjacency checks |
| QTO geometry | Volume and area from real tessellated solids |
| Space analysis | Space volume extraction from `IfcSpace` bodies |
| Geometry export (OBJ/GLTF) | Raw tessellated vertex/index buffers |
| OMG/FOG geometry | Bounding boxes, geometry references |
| Visualization sidecar | Full mesh export |

The crate exposes:
- `FlatMesh` — per-element geometry: list of `GeometryInstance { vertices, normals, index, color, transform }`
- `IfcGeometry` — raw tessellated mesh (vertex buffer + index buffer)
- `stream_meshes(step, ids, callback)` — iterate all elements, same interface as web-ifc
- `get_flat_mesh(step, element_id)` — single element geometry

---

## Source Files (read in this order before porting each phase)

| File | Lines | Purpose |
|---|---|---|
| `representation/geometry.h` | 325 | Core data structures |
| `representation/IfcGeometry.cpp` | 311 | IfcGeometry implementation |
| `operations/geometryutils.h` | 1007 | Math helpers, mesh utils |
| `operations/mesh_utils.h` | 477 | Vertex dedup, normal computation |
| `operations/bim-geometry/utils.h` | 1802 | Shape tessellation (extrusion, facets, revolve) |
| `operations/bim-geometry/geometry.cpp` | 276 | Shape tessellation helpers |
| `operations/curve-utils.h` | 510 | Profile and curve extraction |
| `operations/IfcCurve.cpp` | large | All IFC curve/profile types |
| `operations/boolean-utils/math.h` | 700 | Boolean math primitives |
| `operations/boolean-utils/shared-position.h` | 2074 | CSG operations |
| `operations/boolean-utils/clip-mesh.h` | 360 | Half-space clipping |
| `IfcGeometryProcessor.cpp` | 2182 | Per-type dispatch |
| `IfcGeometryLoader.cpp` | 4311 | StreamMeshes, GetFlatMesh |

---

## Port Phases

### Phase 1 — Data structures

**Source**: `representation/geometry.h`, `representation/IfcGeometry.cpp`

Port `IfcGeometry`, `IfcMesh`, `IfcComposedMesh`, `IfcSurface`, `IfcCurve`.
These are the containers that all later phases fill.

```rust
pub struct IfcGeometry {
    pub vertices: Vec<f64>,  // 6 floats per vertex: x,y,z,nx,ny,nz
    pub indices: Vec<u32>,
}

pub struct GeometryInstance {
    pub geometry: IfcGeometry,
    pub color: [f32; 4],           // RGBA 0-1
    pub flat_transformation: [f64; 16], // 4x4 col-major world matrix
    pub geometry_express_id: u64,
}

pub struct FlatMesh {
    pub express_id: u64,
    pub geometries: Vec<GeometryInstance>,
}
```

### Phase 2 — Geometry utilities

**Source**: `operations/geometryutils.h`, `operations/mesh_utils.h`

Port:
- `cross`, `dot`, `normalize`, `length` (3D vector math)
- `GetBoundingBox(geometry)` → `[f64; 6]` (min/max)
- `VertexMap` — dedup vertices by rounded coordinate key
- `ComputeVertexNormals(geometry)` — average face normals per vertex
- `IsDegenerate(triangle)` — zero-area triangle check
- `Triangulate(polygon)` → triangle list (ear-clipping for complex polygons)

### Phase 3 — Shape tessellation (highest priority)

**Source**: `operations/bim-geometry/utils.h`, `geometry.cpp`

Port in this order (most common first in sample.ifc):

#### 3a. `GetPolygonalFaceSet` (IFCPOLYGONALFACESET)

```
IFCPOLYGONALFACESET(Coordinates, Closed, Faces, PnIndex)
```

Read from C++ at `IfcGeometryProcessor.cpp:594`.
Key: vertex list from `IFCCOORDINATELIST3D`, face indices from `IFCINDEXEDPOLYGONALFACE`.

#### 3b. `GetExtrudedAreaSolid` (IFCEXTRUDEDAREASOLID)

```
IFCEXTRUDEDAREASOLID(SweptArea, Position, ExtrudedDirection, Depth)
```

Read from C++ at `IfcGeometryProcessor.cpp:947`.
Steps (same as C++):
1. Get profile curve from `SweptArea`
2. Apply `Position` placement
3. Extrude along `ExtrudedDirection` by `Depth`
4. Cap top and bottom, triangulate sides

#### 3c. `GetTriangulatedFaceSet` (IFCTRIANGULATEDFACESET)

```
IFCTRIANGULATEDFACESET(Coordinates, Normals, Closed, CoordIndex, PnIndex)
```

Direct triangle list — simplest case.

#### 3d. `GetFacetedBrep` (IFCFACETEDBREP)

Read closed shell faces and triangulate each polygon face.

#### 3e. `GetRevolveSolid` (IFCREVOLVEDAREASOLID)

Revolve a profile around an axis by a given angle.
Needed for the currently-missing 249 mesh_items.

#### 3f. `GetSweptDiskSolid` (IFCSWEPTDISKSOLID)

Sweep a circle profile along a directrix curve.
Handled specially as `CircleExtrusion` in fragments format.

### Phase 4 — Curves and profiles

**Source**: `operations/curve-utils.h`, `operations/IfcCurve.cpp`

Port all profile types (feed into Phase 3 tessellation):
- `GetArbitraryClosedProfile` (IFCARBITRARYCLOSEDPROFILEDEF) — polygon outline from polyline/curve
- `GetArbitraryProfileWithVoids` — outer profile + holes
- `GetRectangleProfile` (IFCRECTANGLEPROFILEDEF)
- `GetCircleProfile` (IFCCIRCLEPROFILEDEF, IFCCIRCLEHOLLOWPROFILEDEF)
- `GetIProfile`, `GetTProfile`, `GetLProfile`, `GetZProfile`, `GetCProfile`, `GetUProfile`

Curve types (used in profile definitions):
- `GetPolyline` (IFCPOLYLINE)
- `GetIndexedPolyCurve` (IFCINDEXEDPOLYCURVE)
- `GetCompositeCurve` (IFCCOMPOSITECURVE)
- `GetTrimmedCurve` (IFCTRIMMEDCURVE)

### Phase 5 — Boolean operations

**Source**: `operations/boolean-utils/`

Port `IfcBooleanResult` and `IfcBooleanClippingResult`:
- `ClipMeshByPlane` (half-space solid clipping)
- `CSGUnion`, `CSGDifference`, `CSGIntersection`

This is the most complex phase. The `shared-position.h` (~2074 lines) is the core.

### Phase 6 — Main dispatcher

**Source**: `IfcGeometryProcessor.cpp`, `IfcGeometryLoader.cpp`

Port:
- `GetMesh(step, express_id)` — dispatch to per-type handlers (Phases 3-5)
- `GetFlatMesh(step, element_id)` — get geometry for one element with world transforms applied
- `stream_meshes(step, ids, callback)` — same as web-ifc's `StreamMeshes`
- `GetCoordinationMatrix(step)` — model-level coordinate transform (COORDINATE_TO_ORIGIN)
- Color extraction from `IFCSTYLEDITEM` chain (currently in `fragments-core/convert.rs`, move here)

---

## Integration into `fragments-core`

After Phase 6 is done, replace `step.rs` geometry extraction with `fragments_geometry`:

```rust
// fragments-core/src/convert.rs
use fragments_geometry::stream_meshes;

stream_meshes(&step, &element_ids, |flat_mesh| {
    // flat_mesh.geometries has vertex buffers, colors, transforms
    // same structure as web-ifc FlatMesh
    // feed directly into get_shell_data + build_meshes
});
```

The `build_meshes` function already calls `get_shell_data` (oracle's shell processor).
After this integration, the entire pipeline matches the oracle exactly.

---

## Port Rules

- **Read C++ source first, then port** — no inventing logic
- **Same precision constants** — web-ifc uses `1e-6` for most tolerances
- **Same data layout** — vertex buffer as `Vec<f64>` (6 floats: xyz + normal xyz)
- **No counting output to verify progress** — compile, run tests, check rendering
- **WASM-compatible** — no filesystem access, no threading primitives not supported in WASM

---

## Milestone Tests

After each phase, the test is:
```bash
cargo test -p fragments-geometry --lib
cargo test -p fragments-core --lib
```

Integration test: compare geometry counts on sample.ifc using `frag-diff`.
Full parity check: `python3 scripts/validate_fragments_parity.py web/wasm-prototype/public/sample.ifc`

---

## Upstream Reference

```
/tmp/web-ifc/src/cpp/web-ifc/geometry/IfcGeometryProcessor.cpp  (2182 lines - main dispatch)
/tmp/web-ifc/src/cpp/web-ifc/geometry/IfcGeometryLoader.cpp      (4311 lines - element iteration)
/tmp/web-ifc/src/cpp/web-ifc/geometry/operations/               (geometry math)
/tmp/web-ifc/src/cpp/web-ifc/geometry/representation/           (data structures)
```
