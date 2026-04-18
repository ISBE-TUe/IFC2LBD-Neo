# Replace csgrs with parry3d for Mesh Intersection Detection

## Problem

The `csgrs` CSG library (BSP-tree based) crashes on real IFC meshes:
- **Stack overflow** on meshes with >200 triangles due to unbounded BSP recursion depth
- **486 triangles** crashed even with 32MB thread stack isolation
- Hard limits (200/500/2000 triangles) cause silent fallback to bbox, losing topology precision

## Solution: parry3d

`parry3d` is a mature collision detection library by dimforge (Rapier physics authors):
- 1.5M+ crates.io downloads
- Pure Rust, WASM-compatible
- Built for real-world meshes (game engines, robotics)
- **No recursion depth limits** — uses iterative algorithms

### Why parry3d over other alternatives

| Library | CSG booleans | Collision detection | WASM | Mesh scale |
|---|---|---|---|---|
| **csgrs** | ✅ | ❌ | ✅ | Broken (>200 tris) |
| **manifold3d** | ✅ | ❌ | C++ WASM FFI | Good |
| **baby_shark** | ✅ | ✅ | GPU only | Good |
| **parry3d** | ❌ | ✅ | ✅ | 100K+ triangles |

**Key insight**: BOT topology needs *intersection detection* (boolean OR), not *mesh subtraction* (boolean AND). We only need to know if two elements intersect — not compute the intersection geometry itself. parry3d is purpose-built for this.

## Architecture

### What parry3d provides

1. **AABB broad-phase**: `SAPBroadPhase` + `NaiveBroadPhase` for O(n log n) pair detection
2. **Triangle-triangle narrow-phase**: `triangles_intersect()` for exact overlap test
3. **Point-in-mesh**: `is_inside()` for containment checks
4. **Mesh bounding**: `compute_aabb()` for efficient culling

### What we DON'T need from parry3d

- Full CSG boolean operations (subtract/union/complement)
- Mesh repair or manifold guarantees
- Parametric curve operations

### Migration path

```
Current flow:                    New flow:
┌──────────────┐                 ┌──────────────┐
│ R-tree bbox  │                 │ parry3d AABB │
│ broad-phase  │                 │ broad-phase  │
└──────┬───────┘                 └──────┬───────┘
       │                                │
       ▼                                ▼
┌──────────────┐                 ┌─────────────────┐
│ csgrs CSG    │  ────────►     │ parry3d triangle│
│ boolean AND  │                 │ triangle overlap│
└──────────────┘                 │ test (iterative)│
                                 └─────────────────┘
```

### Implementation steps

1. **Add `parry3d` dependency** to `crates/lbd-geometry/Cargo.toml`
2. **Create new module** `crates/lbd-geometry/src/parry_collider.rs`:
   - Convert `TriangleMesh` → parry3d `TriMesh` collider
   - Triangle-triangle intersection using `parry3d::query::triangles_intersect()`
   - Mesh-mesh intersection via triangle list scan (with AABB pre-filter)
3. **Replace `to_csgrs_csg()` and `csg_boolean_intersection()`** in `csg.rs`:
   - Keep `extract_element_mesh()` and all IFC→mesh extraction (still needed)
   - Replace CSG boolean with parry3d triangle overlap
4. **Update `CsgrsGeometryKernel`** → `ParryGeometryKernel`:
   - Implement `ExactGeometryKernel` trait
   - Single-pair analysis via parry3d collider
5. **Preserve bbox fallback** — unchanged, still used when mesh extraction fails
6. **Update batched analysis** `derive_relations_with_csg_batched()` — same pattern, just uses parry3d intersection

### API surface (unchanged externally)

```rust
// Public API stays the same — only internals change
pub fn derive_relations_with_csg(
    model: &IfcModel,
    step: &StepFile,
    candidate_pairs: &[(EntityId, EntityId)],
    options: &ExactCheckOptions,
    fallback_bboxes: &HashMap<EntityId, [f64; 6]>,
) -> Vec<GeometryRelation>;

pub trait ExactGeometryKernel {
    fn analyze_pair(
        &self,
        model: &IfcModel,
        left: EntityId,
        right: EntityId,
        options: &ExactCheckOptions,
    ) -> Result<ExactPairAnalysis, GeometryKernelError>;
}
```

### Performance expectations

- **Triangle-triangle check**: O(1) — single parry3d function call
- **Mesh-mesh intersection**: O(n×m) worst case, but n,m per element are typically 100-5000 (not 100K+)
- **No recursion** — iterative sweep, constant stack depth
- **WASM compatible** — no FFI, no C++

### Risks & mitigations

| Risk | Mitigation |
|---|---|
| parry3d triangle count still high | Use parry3d `SAPBroadPhase` to skip non-overlapping pairs first |
| Winding order / normal issues | parry3d `triangles_intersect` is winding-independent (uses SAT) |
| Coordinate precision | parry3d works in f32 — convert f64 mesh to f32, scale to reasonable world units |

### Files to modify

1. `crates/lbd-geometry/Cargo.toml` — replace `csgrs` with `parry3d`
2. `crates/lbd-geometry/src/csg.rs` — replace boolean logic, keep mesh extraction
3. `crates/lbd-geometry/src/lib.rs` — no changes (same exports)
4. `crates/lbd-pipeline/src/lib.rs` — no changes (uses trait interface)

### Files to keep unchanged

- All IFC→mesh extraction functions in `csg.rs` (200+ lines of STEP parser code)
- `Affine3` transform type and helper functions
- `TriangleMesh` type and mesh assembly functions
- Bbox fallback logic
- `ExactGeometryKernel` trait definition
