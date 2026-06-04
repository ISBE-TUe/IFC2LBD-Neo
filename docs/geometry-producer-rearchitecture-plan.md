# Geometry Producer Rearchitecture — Coverage, Dedup & Parity

**Status:** IN PROGRESS
**Goal:** maximum element coverage, smallest fragment size (best shell dedup), and
best oracle parity — the foundation for a high-performance web viewer.

## Diagnosis (measured, `IFC_DEDUP_STATS=1`)

### TUX (production structural model, 44 MB)

```
selected_elements      = 30236
tessellated_in_ifclite = 17678
empty (no ifc-lite geo)= 12558   ← exactly the 12558 IFCREINFORCINGBAR
fragments emitted      =  1455   ← only 8% of what ifc-lite produced!
unique_shells          =    24
```

### model A (architectural model, 8.6 MB)

```
elements=769 samples=3855 unique_shells=1493   (oracle web-ifc: 662)
by category: IfcRailing 768 (13 elems!), IfcWall 234, IfcDoor 119, ...
```

## Three problems, priority order

### #1 — Fragments producer drops 92% of tessellated geometry  (KEYSTONE)

`build_fragments`/`build_meshes` in `plugin-geometry-producer` iterates ifc-lite's
`TessellatedModel`, but for each element it calls **fragments-core**'s
`geometry_instances_for_product(step, element_id)` to enumerate instances and derive
transforms. fragments-core's STEP walk (`collect_geometry_instances` /
`shell_from_item_direct`) only recognises `IFCEXTRUDEDAREASOLID`, `IFCFACETEDBREP`,
`IFCMAPPEDITEM`, `IFCBOOLEANCLIPPINGRESULT`. Every element whose body is another type
(swept solids, advanced breps, sectioned solids, …) returns no instances and is
**silently dropped — even though ifc-lite already tessellated it**.

Result on TUX: 17678 tessellated → 1455 emitted.

**Fix:** make the fragments producer **ifc-lite-native**. Enumerate shells and
placements from ifc-lite's `TessellatedModel` (`FlatMesh` → `GeometryInstance` with
`world_transform` + `local_transform`, already computed and — post un-bake fix —
correct), instead of re-deriving them from a second STEP traversal. Drop the
fragments-core dependency from this path entirely (we already deleted the standalone
`fragments-producer`).

This single change: recovers coverage, removes the legacy engine, and makes dedup key
on the ifc-lite mesh natively (`hash_ifc_mesh`, already written).

### #2 — Reinforcing bars (SweptDiskSolid) tessellate empty

12558 `IFCREINFORCINGBAR` use `IFCSWEPTDISKSOLID`. ifc-lite has a
`SweptDiskSolidProcessor` but it yields empty meshes for TUX rebar. Needs
investigation: directrix type (likely `IFCINDEXEDPOLYCURVE`/3D polyline), disk radius
parsing, or unit handling. Recovers all reinforcement geometry.

### #3 — Assembly sub-components don't dedup

Railings/doors/stairs bake each sub-component's placement into its mesh, so identical
balusters/panels at different positions never hash-match. Position-free extraction
currently covers only direct `IFCEXTRUDEDAREASOLID` (the un-bake in
`process_item_definition_space`). Extend position-free handling to mapped items /
assembly components so repeats collapse. Touches vendored ifc-lite; correctness-sensitive.

## Phased plan

| Phase | Work | Success metric |
|-------|------|----------------|
| 1 ✅ | Rearchitect `build_fragments` to be ifc-lite-native (enumerate from `TessellatedModel`; dedup via `hash_ifc_mesh`) | **DONE: TUX 1455 → 17678 emitted; model A 769/1493 unchanged.** fragments-core no longer used by build_meshes. Pending user visual confirmation. |
| 2 ✅ | Investigate + fix `SweptDiskSolid` for rebar | **DONE: TUX tessellated 17678 → 30236 (empty=0).** `IfcTrimmedCurve` over `IfcLine` now evaluated in 3D (`profiles.rs::process_trimmed_line_3d`); all 12558 rebar recovered, 82005 samples → 3310 shells. |
| 3 ✅ | Translation-normalized shell dedup (per-instance min-corner offset folded into transform) | **DONE: model A 1493 → 597 shells (oracle 662 — now at/below parity); railings 768 → 42. TUX unchanged 3309.** Implemented in `plugin-geometry-producer` (no vendored change): `mesh_min_corner`, translation-invariant `hash_ifc_mesh`, `build_shell(offset)`, `local × translate(offset)`. Pending visual verification. |
| 4 | Apply same ifc-lite-native enumeration to glTF + Parquet writers (instancing) | glTF/Parquet size parity with fragments. **Lower priority — secondary export formats; viewer uses fragments.** |

## Shell encoding fix (correctness — required by the ifc-lite-native switch)

Symptom: fragments rendered with giant spanning "red fan" polygons (GLB of the same model
was correct). Root cause: `fragments_core::get_shell_data` regroups triangles into coplanar
polygon *profiles* (boundary-loop extraction) for the fragment format. That tracer cannot
robustly reconstruct face loops from **ifc-lite's arbitrary fan/strip triangulations**, so it
stitched non-coplanar regions into spanning polygons. GLB is immune because it writes raw
triangles and never regroups.

Fix: `build_shell` now emits **raw per-triangle shells** via `get_raw_shell_data` (one
3-vertex profile per triangle, points welded by position) — byte-faithful to the same
triangulation the validated GLB path uses. Correct by construction.

Trade-off: ~8% larger fragments than coplanar-merged shells (664 KB vs 613 KB on model A).
**Future optimization (optional):** make `get_shell_data`'s boundary tracer robust to
arbitrary triangulations (or re-triangulate per coplanar group) to recover the coplanar-merge
size win without the correctness risk.

## Verification per phase

- `IFC_DEDUP_STATS=1` for coverage + shell counts (TUX and model A).
- web-ifc bbox comparison (`/tmp/cmp_slab.js` pattern) for correctness on sampled elements.
- Visual check in the WASM viewer (port 3000) and ThatOpen fragment viewer.
- `scripts/validate_fragments_parity.py` for oracle size parity.

## Invariants (must not regress)

- Element orientation correctness (the un-bake `Rᵀ` fix — verified vs web-ifc).
- No false-positive shell merges (a hash collision merging distinct geometry corrupts
  the model). Keep the bbox + invariant signature conservative.
