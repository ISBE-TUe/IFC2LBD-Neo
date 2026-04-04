# Quad Chunking Plan (`ifc2lbd-neo`)

## Goal
Add first-class N-Quads chunk output in `ifc2lbd-neo` so `singleIngestConverter` can bulk-load chunks directly into Oxigraph with minimal extra processing.

## Why here
- `ifc2lbd-neo` already owns RDF serialization and graph IRI assignment.
- Avoids re-splitting in Python after conversion.
- Makes chunking deterministic, portable, and testable at source.

## CLI design
Add flags to `crates/ifc2lbd-cli/src/main.rs`:
- `--quad-chunking <none|lines|bytes>`
  - default: `none`
- `--quad-chunk-size-lines <N>`
  - used when `--quad-chunking lines`
  - default: `2000000`
- `--quad-chunk-size-bytes <N>`
  - used when `--quad-chunking bytes`
  - default: `268435456` (256 MiB)
- `--quad-chunk-prefix <name>`
  - default: `out`
  - output files: `<prefix>.part-000.nq`, `<prefix>.part-001.nq`, ...
- `--quad-chunk-min-count <N>`
  - optional floor for chunk count in auto scenarios
  - default: `1`

Constraint:
- Chunking flags are valid only with `--output-format nquads`.

## Output contract
When chunking is enabled:
- Emit multiple `.nq` files in the target output directory.
- Keep each line as one full N-Quad (never split a line).
- Keep named graph IRIs exactly as produced today (`lbd_graph_iri`, `ifcowl_graph_iri`).
- Write a small manifest JSON next to chunks:
  - `quad_chunks.manifest.json`
  - includes ordered file list, byte sizes, line counts, total triples estimate.

## Implementation approach
1. Add chunk writer abstraction in CLI output layer.
2. Stream merged N-Quads through writer once (no second pass).
3. Rotate chunk file on threshold (`lines` or `bytes`).
4. Track per-chunk counters while writing.
5. Write manifest on successful completion.
6. Keep current single-file path untouched when `--quad-chunking none`.

## Integration with `singleIngestConverter`
- Update module wrapper to pass chunk flags when backend mode is Oxigraph bulk.
- Prefer loading chunk list from `quad_chunks.manifest.json`.
- Fallback to single `.nq` if manifest not present.

## Performance targets
- Chunking overhead should be near sequential write speed.
- No external split container/tool required.
- Expected improvement vs post-conversion Python split: remove extra read/write pass.

## Validation plan
- Unit tests:
  - flag parsing and invalid combinations.
  - rotation logic for `lines` and `bytes`.
  - manifest correctness.
- Integration tests:
  - small IFC -> multiple `.nq` chunks + manifest.
  - Oxigraph bulk load using produced manifest order.
- Regression:
  - existing `--output-format turtle` and non-chunked `nquads` unchanged.

## Rollout
1. Implement behind `--quad-chunking` (default off).
2. Release new artifact.
3. Switch `singleIngestConverter` Oxigraph mode to chunk-aware load path.
4. Benchmark end-to-end on 1GB, 10GB, 50GB RDF outputs.

## Open decisions
- Keep both strategies (`lines`, `bytes`) or only `bytes`.
- Whether to support optional gzip chunks (`.nq.gz`) later.
- Default chunk size for high-core Linux servers.
