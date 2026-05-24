# bSDD Matching Rework Plan

Status: implemented (all 4 phases). 2026-05-24.
Branch: `feature/bsdd` (current).

This document captures the planned rework of the bSDD producer's matching layer
and supporting preprocessors. The current implementation works for demo files
but has correctness, transparency, and reproducibility problems that will bite
on real project data.

## Why this rework is needed

A non-trivial chunk of bSDD's pain comes from buildingSMART itself, not from
our code. Designing around that reality is part of this plan.

- bSDD property identifiers are not canonical strings. The same conceptual
  property can appear as `FireRating`, `Fire Rating`, `firerating`, etc.
  Every consumer has to invent its own normalization.
- The bSDD class hierarchy is inconsistent. `IFCWALLSTANDARDCASE` has no
  canonical bSDD entry and is modeled as `IfcWall`. Translations like this
  are scattered and undocumented.
- The same property can live under different psets across classes, and the
  bSDD API does not declare a primary. Ambiguous matches are unavoidable.
- The bSDD search API is slow and lossy. Local indexes are necessary.
- There is no official "this software's property maps to this bSDD prop"
  registry. Each consumer maintains its own mapping.

Conclusion: assume the bSDD side will keep being inconsistent and design our
layer to absorb that.

## Semantic structure assessment (bsddc / bsddp / bsddm)

The three-namespace split is correct and should be kept:

- `bsddc/` (`https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/class/`)
  — official bSDD class IRIs.
- `bsddp/` (`https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/prop/`)
  — official bSDD property IRIs.
- `bsddm:` (`https://w3id.org/ifc2lbd/bsdd-meta#`) — our own provenance /
  mapping-status layer.

The principle (their IRIs for things bSDD owns; our IRIs for things we say
about how we did the mapping) is sound. Consumers who trust the mapping can
ignore the meta layer entirely; consumers who don't can audit it.

### Small cleanups to do as part of Phase 1

Both are cheap and worth doing once we're touching this code.

1. **Case convention in `bsddm:` is inconsistent.**
   Today we have both `bsddm:Property` (PascalCase, used as a class) and
   `bsddm:customProperty` / `bsddm:customPropertySet` / `bsddm:customQuantitySet`
   (camelCase, also used as classes).
   Convention: classes PascalCase, predicates camelCase.
   Rename:
   - `bsddm:customProperty` → `bsddm:CustomProperty`
   - `bsddm:customPropertySet` → `bsddm:CustomPropertySet`
   - `bsddm:customQuantitySet` → `bsddm:CustomQuantitySet`

2. **Status values should be a typed SKOS concept scheme.**
   Today `bsddm:Mapped`, `bsddm:Normalized`, `bsddm:Ambiguous`, `bsddm:Unmapped`
   are bare IRIs with no class. Add:
   - `bsddm:MappingStatus a rdfs:Class .`
   - Each status `a bsddm:MappingStatus .` (or model as `skos:Concept` in a
     `skos:ConceptScheme`).
   Five extra triples; enables SHACL/SPARQL queries like "all matching statuses".

## Phase 1 — Stop the bleeding

Goal: the matcher stops lying to consumers. Small, focused PR.

1. **Track ambiguous properly.** Replace `MatchStatus::Ambiguous` (currently
   silent) with `Ambiguous { candidates: Vec<CandidateCode> }`. Emit each
   candidate in RDF with `bsddm:candidateProperty` triples. Let consumers pick.
2. **Make fuzzy class/pset-aware.** Pass class + pset into the fuzzy candidate
   filter; only score against candidates that bSDD says belong to that class.
   If no class-scoped candidates exist, do **not** fall through to global
   fuzzy — emit `Unmapped` instead. Better to admit defeat than mismatch.
3. **Name the magic numbers.** `FUZZY_THRESHOLD = 0.94`,
   `MAX_FUZZY_CANDIDATES = 400` — both pulled out, both moved to the mapping
   profile in Phase 2 as overridable. For Phase 1 they are at least
   named constants with comments.
4. **Three unit tests minimum.** Exact hit, fuzzy near threshold, class-scoped
   ambiguous. These tests must *fail* if anyone moves the threshold without
   thinking.
5. **Strip the global ASCII transliteration out of the cleanup preprocessor.**
   Move it inside the matcher's normalization key. The model keeps `Höhe` in
   its labels; only the matcher's internal lookup key sees `hoehe`.
6. **Semantics cleanups from the section above** (case convention +
   `MappingStatus` typing) — ship in the same PR while we are renaming things.

Estimated effort: 1–2 days.

## Phase 2 — Mapping profiles as real artifacts

Goal: zero hardcoded mappings in code. Everything tunable lives in versioned
files. Per-country, per-software, per-project mapping packs.

Single profile file shape (TOML; could be JSON if preferred):

```toml
profile_id        = "revit-dach-2024"
profile_version   = "1.2.0"
extends           = "base"            # optional inheritance
bsdd_index_version = "2024-Q4"        # required, fails if mismatch

[fuzzy]
enabled   = true
threshold = 0.94
scope     = "class"                   # never | property | class | pset

[normalization]
software_prefixes = ["BSDP_", "PSET_REVIT_"]
transliteration   = { ä = "ae", ö = "oe", ü = "ue", ß = "ss" }

[class_aliases]
IFCWALLSTANDARDCASE = "IfcWall"

[pset_aliases]
# ...

[prop_aliases]
# ...

[[hard_overrides]]
schema = "IFC4X3"
class  = "IfcBeam"
pset   = "Pset_BeamCommon"
prop   = "IsExternal"
code   = "IsExternal"
```

Rules:

- **Profile loading order**: `base` → `country` → `software` → `project`.
  Each layer can override the previous. So
  `base + de + revit-dach-2024 + my-project-overrides.toml` is a valid stack.
- **No hardcoded fallbacks left in code.** `IFCWALLSTANDARDCASE → IfcWall`
  moves into `base.class_aliases`. Same for every other in-code alias.
- **CLI/WASM accept** `--bsdd-profile path/to/profile.toml` (repeatable,
  layered) or a built-in profile name.
- **Ship a few starter profiles** in
  `crates/lbd-converter/resources/bsdd-profiles/`:
  - `base.toml` — the universal aliases extracted from current code + JSON.
  - `revit-dach.toml`
  - `allplan-de.toml`
  - `tekla-en.toml`

  These are the data we already have, just relocated. Community can extend.

Estimated effort: 2–3 days.

## Phase 3 — Provenance & reproducibility

Goal: identical input + identical profile = identical output, and you can
prove it.

1. **Version-stamp the embedded bSDD index.** Add `index_version` and
   `index_built_at` to the gz blob. Read it on init.
2. **Emit a provenance block per conversion run** as RDF triples in the meta
   graph:
   - `bsddm:ConversionRun bsddm:indexVersion "2024-Q4" .`
   - `bsddm:ConversionRun bsddm:profileId "revit-dach-2024" .`
   - `bsddm:ConversionRun bsddm:profileVersion "1.2.0" .`
   - `bsddm:ConversionRun bsddm:fuzzyThreshold "0.94"^^xsd:double .`
3. **Also include in stats JSON**, so downstream tooling can compare runs.
4. **Fail loudly on index/profile version mismatch.** If a profile declares
   `bsdd_index_version = "2024-Q4"` and the embedded index is `2024-Q2`,
   error out (unless `--accept-version-skew`).

Estimated effort: half a day.

## Phase 4 — Iterative profile selection (experimental)

Goal: given an unfamiliar IFC file, try several profiles and recommend the
best one. Only worth building after Phases 1–3 are solid.

1. **Sampling mode.**
   `ifc2lbd analyze-bsdd input.ifc --candidate-profiles base,revit-dach,allplan-de,tekla-en`
2. Run the matcher on a representative sample (e.g. first 500 properties)
   against each profile.
3. Score each profile by:
   - `matched_ratio` — primary
   - `avg_confidence` — secondary
   - `ambiguous_ratio` — penalty
   - `unmapped_ratio` — penalty
4. Print a ranked table:

   ```
   profile           matched  avg_conf  ambig  unmapped
   revit-dach        87.3%    0.97      4.1%   8.6%
   allplan-de        72.1%    0.95      9.0%   18.9%
   base              45.2%    0.92      11.0%  43.8%
   ```

5. Let the user pick, or auto-pick if a profile is clearly best
   (>15% margin).

Bonus: this becomes the basis for an *improvement loop* — unmapped properties
from a real run feed back into the profile as suggested aliases.

Estimated effort: ~2 days.

## Order of attack

Not one PR. Phased.

- **PR 1 — Phase 1.** Small, focused, makes the matcher honest. 1–2 days.
- **PR 2 — Phase 2.** Bigger, but mostly relocation of existing data plus a
  new profile loader. 2–3 days.
- **PR 3 — Phase 3.** Half a day. Stamping and emitting.
- **PR 4 — Phase 4.** Whenever appetite returns. ~2 days.

Phase 1 alone fixes the "semantically dumb" behavior where `IsExternal` on a
wall matches `IsExternal` on a door identically. That is the highest-leverage
single change in this whole plan.
