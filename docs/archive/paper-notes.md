# IFC2LBD Rust Rewrite - Paper Notes (Archive)

This file is preserved only as historical context.

The current source of truth for paper planning is:

- [`docs/current/paper-plan.md`](../current/paper-plan.md)

## Why This File Is Archived

The previous version of this note captured useful thinking, but parts of it drifted away from the current implementation.

Known examples:

- it no longer reflected the active `--topology-full` code path accurately
- it described some property/quantity-set modeling as future work even though parts are already emitted
- it used wording that could be misread as current architecture truth rather than historical planning context

## How To Use This File Now

- use it only for historical context and idea recovery
- do not copy architecture claims from here into the paper without re-checking code and current docs
- prefer the following truth order for manuscript work:
  1. code
  2. `README.md`
  3. `docs/current/*`
  4. archive/reference notes

## Historical Themes Still Worth Preserving

- respectful framing of the Rust work as part of the Java IFCtoLBD lineage
- focus on parity, comparison, and measured behavior
- emphasis on future topology, geometry, and modeling work
- interest in stronger extensibility for future conversion additions
