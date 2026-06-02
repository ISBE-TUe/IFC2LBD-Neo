# SUPERSEDED — do not use

The fragments producer work documented here is being replaced by the new geometry module architecture.

See `geometry-module-plan.md` for the current plan.

## What was done (for historical reference only)

- `crates/fragments-schema` — FlatBuffers schema crate, keep as-is, used by `plugin-geometry-producer`
- `crates/plugin-fragments-producer` — will be deprecated once `plugin-geometry-producer` is working
- `crates/fragments-core` — geometry extraction code, will be replaced by `plugin-geometry-preprocess` + `crates/ifc-geometry`
- `scripts/validate_fragments_parity.py` — parity harness, stays useful
- `crates/fragments-core/src/bin/frag_diff.rs` — structural diff tool, stays useful

## Parity status at time of supersession (DigitalHub.ifc)

Entity/metadata layer:
- local_ids: 24,444 vs oracle 24,446
- guids: 9,837 — exact match
- relations: 10,925 vs oracle 10,936

Geometry layer (incomplete, will be fully replaced by ifc-lite):
- mesh_items: 769 — exact match
- samples: 3,855 — exact match
- shells: 1,502 vs oracle 662 — dedup not matching oracle
- materials: 26 — exact match
- local_transforms: 595 vs oracle 1,499

The entity/metadata layer work is correct and carries forward into `plugin-geometry-producer`.
The geometry extraction is replaced wholesale by `crates/ifc-geometry` (ifc-lite port).
