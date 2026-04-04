# Full Topology Workflow (Current)

This document describes what happens today when running the CLI with topology-related flags.

Relevant flags:

- `--topology`
- `--topology-full`
- `--bbox`

## Behavior Matrix

- no topology flag:
  - no topology graph build
  - fallback containment logic only
- `--topology` enabled:
  - build topology graph
  - no geometry adjacency derivation
  - no geometry-derived bounding boxes
- `--topology-full` enabled:
  - build topology graph
  - derive geometry relations with the OCC exact-kernel subprocess workflow in CLI
  - pass `geometry_relations` to converter
- `--bbox` enabled:
  - emit bbox geometry resources in LBD:
    - element `lbd:hasBoundingBox` geometry-node
    - element `geo:hasGeometry` geometry-node
    - geometry-node `a geo:Geometry`
    - geometry-node `geo:asWKT "POLYHEDRALSURFACE Z (...)"^^geo:wktLiteral`
  - bbox generation uses hybrid fallback:
    - fast: transformed local AABB
    - fallback (inflation > threshold): rotated XY OBB WKT + Z extent
  - hidden dev threshold: `--bbox-inflation-threshold` (default `1.5`)

## Sequence Diagram

```mermaid
sequenceDiagram
    participant U as User CLI
    participant C as ifc2lbd-cli
    participant S as ifc-step
    participant M as ifc-model
    participant G as OCC exact-kernel path
    participant X as lbd-converter
    participant T as lbd-topology
    participant L as LBD serializer
    participant I as IfcOWL serializer

    U->>C: run with flags
    C->>S: parse_step_file(input)
    S-->>C: StepFile
    C->>M: build_model(step)
    M-->>C: IfcModel

    alt topology-full
        C->>G: topology_full_occ_relations(model, step, input)
        G-->>C: geometry_relations + mesh_bboxes + mesh_wkts
    end

    C->>X: stream_step_and_model(step, model, ConvertOptions)

    par LBD path
        X->>T: build_topology(_with_enricher)
        alt geometry_relations present
            X->>X: merge_geometry_relations_into_topology(..., exact=true)
        end
        X->>X: emit BOT triples + properties + bbox geometries + attributes
        X-->>L: LBD triple batches
    and IfcOWL sidecar path (if enabled)
        X-->>I: IfcOWL triple batches
    end
```

## Current Internal Steps (Full Topology)

1. CLI parses IFC STEP and builds typed IFC model.
2. CLI optionally computes OCC exact-kernel geometry relations for `--topology-full`.
3. CLI builds `ConvertOptions` and streams to converter.
4. Converter emits core entities.
5. Converter builds topology graph from IFC relations:
   - `IfcRelAggregates` -> `ContainsZone`
   - `IfcRelContainedInSpatialStructure` -> `ContainsElement`
   - `IfcRelSpaceBoundary` -> `AdjacentElement` and `AdjacentZone`
   - `IfcRelVoidsElement` + `IfcRelFillsElement` -> `HasSubElement`
6. Converter optionally merges geometry-derived relations.
7. Converter emits BOT topology triples into LBD output.
8. Converter streams IfcOWL sidecar in parallel if `--ifcowl` is enabled.

## Important Current Nuance

`--topology-full` currently uses the OCC exact-kernel subprocess path and merges those relations as core topology evidence in converter. The voxel helper code still exists in the CLI as legacy/exploratory implementation material, but it is not the active main path for `--topology-full`.
