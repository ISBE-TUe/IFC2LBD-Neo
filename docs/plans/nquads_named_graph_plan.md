# Native N-Quads + Named Graph Plan

## Goal
Support direct high-throughput ingestion into Oxigraph from `ifc2lbd-neo` without post-processing in wrapper services.

## Scope
- Keep current Turtle output behavior fully backward compatible.
- Add native N-Quads output mode for:
  - LBD output stream
  - IfcOWL sidecar output stream
- Preserve current two-graph model for converter integration:
  - `<base>/lbd`
  - `<base>/ifcowl`

## CLI Design
- Add `--output-format <turtle|nquads>` (default: `turtle`).
- Add optional graph override flags:
  - `--lbd-graph-iri <iri>`
  - `--ifcowl-graph-iri <iri>`
- Default graph IRIs when not provided:
  - `lbd`: `<base-uri>/lbd`
  - `ifcowl`: `<base-uri>/ifcowl`

## Serialization Changes
- In `lbd-serializer`:
  - Add streaming N-Quads serializer that writes each triple with explicit graph term.
  - Reuse existing literal escaping and RDF term handling rules.
- In `ifc2lbd-cli`:
  - Route LBD and IfcOWL serializer threads by selected format.
  - For `turtle`: keep current behavior.
  - For `nquads`: write quads directly, no Turtle intermediate.

## Integration Contract (`singleIngestConverter`)
- Use `--output-format nquads` when Oxigraph fast path is enabled.
- Keep two logical named graphs:
  - LBD (including geometry triples currently embedded in LBD)
  - IfcOWL
- Prefer single merged `.nq` upload in `svc-convert` for lowest HTTP overhead.

## Performance Expectations
- Remove wrapper-side TTL->NQ conversion CPU cost.
- Reduce disk I/O by avoiding extra temporary conversion files.
- Improve ingest throughput and latency for large models (30M-50M triples).

## Compatibility and Rollout
1. Implement flags + serializers behind default `turtle` mode.
2. Add fixture-based tests to confirm Turtle output parity.
3. Add N-Quads smoke tests validating graph assignment.
4. Switch converter deployment to native N-Quads mode.
5. Keep wrapper conversion path as fallback during rollout.

## Open Decisions
- Whether to add an optional `--merge-nquads` single-file mode directly in CLI.
- Whether to expose a third graph flag later if geometry ever becomes a dedicated sidecar stream again.
