# Topology Bug Note (2026-03-17)

## What is broken

- Geometry-derived topology output is over-connected and semantically noisy.
- `bot:adjacentElement` currently links implausible pairs (door-door, door-window, window-window in many cases).
- `bot:Interface` is emitted too aggressively and does not represent clean, trusted building interfaces yet.
- Viewer sanity checks fail: one anchor door returns too many unrelated neighbors, so the graph is not usable for validation or downstream logic.
- Query support delivered during debugging was not focused enough for per-anchor sanity workflows.

## Concrete bad example

- Anchor door GUID: `1hOSvn6df7F8_7GcBWlSDm`
- Returned neighbors include multiple doors, windows, slabs, and walls at once via interface/adjacency.
- This indicates relation inflation and weak semantic filtering in the current topology pipeline.

## Why this is a problem

- BOT topology currently cannot be trusted as a precise semantic surface.
- False positives make pathing, containment sanity checks, and room-level analytics unreliable.
- The output is difficult to inspect in 3D because result sets are polluted.

## What must be fixed next

- Restrict interface/adjacency emission with strict semantic pair rules and geometric validation gates.
- Separate raw geometry contacts from curated BOT semantics (do not expose raw contacts as final BOT facts).
- Add deterministic per-anchor sanity tests (door, wall, space) with expected neighbor class constraints.
- Re-run Duplex and verify that anchor queries return plausible local neighborhoods only.
