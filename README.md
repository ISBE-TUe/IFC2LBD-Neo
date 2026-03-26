# IFC2LBD-Neo

Rust workspace for converting IFC STEP files into LBD Turtle (plus optional IfcOWL and topology sidecar output).

## Run

```bash
cargo run -p ifc2lbd-cli --bin ifc2lbd-neo -- \
  path/to/model.ifc \
  --output out/lbd.ttl \
  --base-uri https://example.test/base/ \
  --ifcowl \
  --topology
```

Key flags:
- `--output` (alias: `--target-file`)
- `--base-uri` (alias: `--url`)
- `--ifcowl` (writes `<output_stem>_ifcowl.ttl`)
- `--topology` (IFC-relation topology only)
- `--topology-full` (advanced mode; currently voxel geometry adjacency)
- `--bbox` (emit bounding-box geometries as WKT)

## Modes

- Basic LBD only:
  - `ifc2lbd-neo model.ifc --output out.ttl`
- LBD + IfcOWL sidecar:
  - `ifc2lbd-neo model.ifc --output out.ttl --ifcowl`
- Topology from IFC relations only:
  - `ifc2lbd-neo model.ifc --output out.ttl --topology`
- Full topology mode:
  - `ifc2lbd-neo model.ifc --output out.ttl --topology-full`
- Bounding boxes only:
  - `ifc2lbd-neo model.ifc --output out.ttl --bbox`
- Full topology + bounding boxes:
  - `ifc2lbd-neo model.ifc --output out.ttl --topology-full --bbox`

Output behavior:
- `--output` always writes the LBD file.
- `--ifcowl` always writes a separate sidecar file named `<output_stem>_ifcowl.ttl`.
- Topology triples are emitted in the LBD file when `--topology` or `--topology-full` is enabled.
- Bounding boxes are emitted only when `--bbox` is set.
- BBoxes are emitted as geometry resources linked via `lbd:hasBoundingBox` and `geo:hasGeometry`, with `geo:asWKT` (`POLYHEDRALSURFACE Z`).
- Hidden dev tuning is available for bbox fallback threshold: `--bbox-inflation-threshold` (default `1.5`).

## Code Map

- `crates/ifc-step`: STEP parser.
- `crates/ifc-model`: typed IFC model + relationship indexes.
- `crates/lbd-topology`: topology graph derivation.
- `crates/lbd-geometry`: bbox/exact-kernel topology enrichment.
- `crates/lbd-converter`: IFC model -> LBD/IfcOWL triples.
- `crates/lbd-serializer`: streaming Turtle serialization.
- `crates/ifc2lbd-cli`: executable entrypoint and CLI orchestration.

## Validation

```bash
cargo test
python3 scripts/run_release_benchmarks.py
python3 scripts/run_allowed_fixtures.py
```

Benchmark scripts automatically skip fixture files that are not present.

## Converter Docs

- Docs index: `docs/README.md`
- Documentation status matrix: `docs/current/status.md`
- Pipeline modularization and extension guide: `docs/current/converter-pipeline.md`
- Contributing standards: `docs/current/contributing.md`
- Testing and benchmarking guide: `docs/current/testing-and-benchmarking.md`
- WebAssembly plan (reviewed): `docs/current/future-wasm-plan.md`
