# Geometry Kernel Adapter Contract

## Purpose

Define the production contract for the external exact-geometry kernel used by `ifc2lbd-neo`.

- broad phase: bbox candidate pairs in Rust
- narrow phase: exact geometry checks in an external kernel executable

The converter treats the kernel as required infrastructure for exact mode.
When `--geometry-bboxes-file` is used, exact kernel is required by default.

In-repo kernel binary:

- crate: `crates/lbd-geometry-kernel`
- binary: `lbd-geometry-kernel`
- backend: native Rust + OpenCascade (`chijin`)

## CLI Integration

```bash
cargo run -p ifc2lbd-cli --bin ifc2lbd-neo -- \
  Duplex.ifc \
  -t /tmp/duplex_lbd.ttl \
  --topology-file /tmp/duplex_topology.ttl \
  --geometry-bboxes-file /tmp/duplex_bboxes.json \
  --exact-kernel-bin /path/to/your/exact-kernel
```

Kernel expects per-entity BRep cache files by default at:

- `<ifc_path>.occ-cache/<entity_id>.brepbin`

Optional kernel-side override:

- `lbd-geometry-kernel --brep-cache-dir /path/to/cache`

For explicit non-exact fallback (not recommended for production precision), add:

```bash
--allow-bbox-only-geometry
```

## Batch JSON Contract

Kernel stdin request:

```json
{
  "ifc_path": "/abs/path/model.ifc",
  "tolerance": 1e-6,
  "pairs": [
    { "left": 4131, "right": 4287 }
  ]
}
```

Kernel stdout response:

```json
{
  "results": [
    {
      "left": 4131,
      "right": 4287,
      "intersects": true,
      "touches_within_tolerance": false,
      "minimum_distance": 0.0,
      "interface": null,
      "error": null
    }
  ]
}
```

## Semantics

- `intersects = true` -> emit `bot:intersectingElement` (both directions).
- `touches_within_tolerance = true` and `intersects = false` -> emit `bot:adjacentElement` (both directions).
- `interface != null` -> candidate for `topo:interfaceOf` / later `bot:Interface` projection.
- `error != null` -> conversion fails fast.
- missing pair results in batch response -> conversion fails fast.

## Production Requirements

- Kernel must be deterministic for fixed inputs.
- Kernel must return one result per requested pair.
- Kernel must complete within converter-internal timeout per batch.
- Kernel must use exact solid/surface checks, not bbox-only approximations.
- Kernel is expected to be prebuilt and reused across runs (external executable path), so normal converter runs do not recompile OCC.
