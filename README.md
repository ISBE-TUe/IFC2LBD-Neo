# IFC2LBD-Neo

Rust workspace for converting IFC STEP files into LBD Turtle or N-Quads (plus optional IfcOWL sidecar in Turtle mode).

## Run

```bash
cargo run -p ifc2lbd-cli --bin ifc2lbd-neo -- \
  path/to/model.ifc \
  --output out/lbd.nq \
  --base-uri https://example.test/base/ \
  --output-format nquads \
  --topology
```

Key flags:
- `--output` (alias: `--target-file`)
- `--base-uri` (alias: `--url`)
- `--output-format <turtle|nquads>` (default: `turtle`)
- `--lbd-graph-iri` (nquads mode, default: `<base-uri>/lbd`)
- `--ifcowl-graph-iri` (nquads mode, default: `<base-uri>/ifcowl`)
- `--quad-chunking <none|lines|bytes|cores>` (nquads mode only, default: `none`)
- `--quad-chunk-size-lines <N>` (when `quad-chunking=lines`, default: `2000000`)
- `--quad-chunk-size-bytes <N>` (when `quad-chunking=bytes`, default: `268435456`)
- `--quad-chunk-prefix <name>` (chunk files: `<prefix>.part-000.nq`, ...)
- `--quad-chunk-min-count <N>` (minimum chunk floor metadata)
- `--quad-chunk-core-count <N>` (when `quad-chunking=cores`, override auto core count)
- `--ifcowl` (writes `<output_stem>_ifcowl.ttl`)
- `--topology` (IFC-relation topology only)
- `--topology-full` (advanced mode; currently voxel geometry adjacency)
- `--bbox` (emit bounding-box geometries as WKT)

## Modes

- Basic LBD only:
  - `ifc2lbd-neo model.ifc --output out.ttl`
- LBD + IfcOWL sidecar:
  - `ifc2lbd-neo model.ifc --output out.ttl --ifcowl`
- Single-file N-Quads (LBD + IfcOWL named graphs):
  - `ifc2lbd-neo model.ifc --output out.nq --output-format nquads`
- N-Quads chunking by bytes:
  - `ifc2lbd-neo model.ifc --output out.nq --output-format nquads --quad-chunking bytes --quad-chunk-size-bytes 268435456`
- N-Quads chunking by core count:
  - `ifc2lbd-neo model.ifc --output out.nq --output-format nquads --quad-chunking cores --quad-chunk-core-count 8`
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
- In `turtle` mode:
  - `--output` writes LBD Turtle.
  - `--ifcowl` writes a separate sidecar file named `<output_stem>_ifcowl.ttl`.
- In `nquads` mode:
  - `--output` writes one `.nq` stream with two named graphs (LBD + IfcOWL).
  - `--ifcowl` is not required; IfcOWL emission is enabled automatically for two-graph output.
  - Graph IRIs default to `<base-uri>/lbd` and `<base-uri>/ifcowl`, overridable with `--lbd-graph-iri` and `--ifcowl-graph-iri`.
  - When `--quad-chunking` is enabled, output is written as multiple `.nq` chunk files plus `quad_chunks.manifest.json` in the output directory.
  - `lines`: rotate by line count; `bytes`: rotate by byte threshold (line-safe).
  - `cores`: auto-create chunks by available parallelism, but cap chunk count so estimated chunk size stays at least ~64 MiB (unless overridden with `--quad-chunk-core-count`).
  - `cores` writing uses batched buffered output with a dedicated writer thread to reduce chunking overhead on large exports.
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
- `crates/lbd-serializer`: streaming Turtle and N-Quads serialization.
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
