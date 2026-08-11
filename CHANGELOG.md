# Changelog

All notable changes to IFC2LBD-Neo are documented in this file.

## [0.7.0]

### Fixed — deprecated `*StandardCase` classes were skipped entirely

The quantity-set table is generated from the bSDD **IFC4x3** index, and IFC4x3
folded `IfcWallStandardCase` and its siblings back into their base classes. IFC2X3
and IFC4 files are full of them — essentially every wall in an ArchiCAD IFC2X3
export is an `IfcWallStandardCase` — so those elements were not degraded, they
were **skipped before geometry was ever touched**: 4,124 walls across two models,
tens of thousands of values never attempted.

An unknown `*StandardCase` / `*ElementedCase` class now resolves to its base
class. Verified against the published IFC2X3 subtype graph: of 731 classes,
exactly one lacks a spec while having an ancestor that has one, and this rule
resolves it — zero misses.

### Fixed — a refused volume no longer discards seven other quantities

Measuring a mesh bailed out entirely when the polyhedral volume could not be
proved, so an unclosed shell cost the element its height, footprint, side area,
surface area, extents and plan rectangle as well — none of which need closure.
A wall with window openings produced *nothing*. Volume is now computed separately
and its failure costs only volume.

### Fixed — identical elements measured differently

Volume and surface area were computed after transforming `f32` vertices into
world space, so the same object at two coordinates quantised differently and
copy-pasted elements disagreed in the 5th-6th significant digit. Both are
invariant under a rigid transform and are now measured in the mesh's own frame;
only extents, shadows and the plan rectangle use the world-placed copy. A
non-rigid (scaled) transform falls back to world measurement.

### Changed — a tessellated volume must be proved, not assumed

A divergence-theorem volume is emitted only where the mesh is a single closed
**orientable** single-component solid from a single segment. Edge parity alone
admitted shells whose winding contradicts itself, whose signed volume looks
perfectly ordinary. Voided elements remain excluded on top of that: on 703 voided
walls the mesh passed all three topology clauses and the volume was still right
only 3.3% of the time, ratios spread 0.004 to 3.8.

### Removed — `Width` and `Height` on doors and windows

Not a gap; a refusal with a measurement behind it. There are three candidate
sources — the nominal `OverallHeight`/`OverallWidth`, the lining, and the authored
figure — and on IFC2X3 the authored value is smaller than both (a clear
dimension, leaf minus frame). The best single rule reproduced **2.3%** of 6,072
`Width` and **1.1%** of 5,930 `Height` values.

A slab's `Width` **is** still emitted, as the nominal thickness IFC defines it to
be, knowingly against a conflict: one exporter means exactly that (99.8% over
1,310 values) and another writes a plan dimension under the same name (0% over
137). No geometric test separates the conventions, so this is a deliberate trade
rather than an oversight.

### Measured

Scored against the quantities already authored in the corpus, which now covers
IFC2X3 as well as IFC4:

| | corpus A (2 IFC4, 3 IFC2X3) | corpus B (2 IFC2X3) |
| --- | ---: | ---: |
| before | 45.4% coverage / 97.7% correct | 13.3% / **54.6%** |
| after | 29.7% / 96.6% | **41.1% / 93.0%** |

IFC2X3 was not previously in the corpus, which is why every defect above survived
a full rebuild unnoticed.

All nine remaining disagreement groups were diagnosed by whether their error is
*systematic* (one factor for every value — the file's convention) or *scattered*
(no rule recovers it):

| group | wrong | median ratio | clustered | verdict |
| --- | ---: | ---: | ---: | --- |
| GrossVolume / Slab | 49 | 10.7639 | 94% | exporter writes **square feet** |
| GrossVolume / Column | 31 | 0.1850 | **100%** | a single exporter constant |
| NetSurfaceArea / Member | 212 | 1.0150 | 74% | joints deducted by the exporter |
| GrossVolume / Wall | 726 | 0.9659 | 33% | scattered |
| Perimeter / Slab | 142 | 0.4674 | 0% | scattered |
| NetArea + NetVolume / Slab | 419 | 0.39 / 0.53 | 13% / 3% | scattered |
| GrossSideArea / Wall | 119 | 1.6033 | 4% | scattered |
| Length / Member | 26 | 0.1881 | 0% | scattered |
| GrossVolume / StairFlight | 16 | 6.3738 | 0% | scattered |

100% clustered means every value differs by the *same* factor, which is the
file's convention rather than our arithmetic. Where a rule held on IFC4 and not
on IFC2X3 — slab `Width`, slab plan areas, slab `Perimeter`, member
`NetSurfaceArea` — the schema is now the discriminator, since no geometric test
separates the conventions. The scattered wall and stair-flight groups are not
addressed and remain the largest known gap.

## [0.6.3]

### Fixed — a type's property set was inherited on top of the occurrence's own

`IfcRelDefinesByType` attached every property set from an element's type to the
element unconditionally, alongside the sets it already carried directly. IFC
treats a type's property sets as **defaults that the occurrence overrides**, so
inheriting both left one object with two same-named sets under two IRIs — and a
property such as `IsExternal` appearing twice, with nothing to say which is in
force.

On a 155 MB IFC2X3 export: **3,703 objects** in that state — 1,589 of them
`Pset_WallCommon` on walls, plus `Pset_DoorCommon` (376), `Pset_SlabCommon` (286)
and twenty other sets. Now **0**.

A type set whose name the occurrence already uses is no longer inherited. The
same rule applies to type quantity sets.

## [0.6.2]

### Fixed — an IFC2X3 file got a second quantity set on every element

IFC4 names a quantity set after its class (`Qto_WallBaseQuantities`); IFC2X3 has
no `Qto_` prefix and exporters write a bare `BaseQuantities` for every class. The
audit compared the set name with an exact, case-sensitive `==`, so in a 2X3 file
it never found the set that was already there and created its own beside it.

Every affected element ended up with **two quantity-set nodes under two IRIs** —
the authored quantities in one, the computed ones in the other — which is what a
viewer shows as the same set twice. On a 96 MB ArchiCAD export: 3,342 sets
created, **0** extended, and 2,226 elements carrying two sets. After the fix,
2,226 extended and **0** elements with two sets; the 1,116 still created are
elements that genuinely have no quantity set in the file.

Set names are now matched trimmed and case-insensitively, and the bare IFC2X3
spelling is accepted, preferring an exact `Qto_<Class>BaseQuantities` where both
are present. The quantity-*name* comparison beside it was already
case-insensitive; the set-name one never was.

### Fixed — created quantity sets had a random identity

A quantity set this module creates takes its GlobalId into its IRI
(`<base>/qs_<guid>`), and that GlobalId was a fresh `Uuid::new_v4()` on every
run. The same file therefore produced different set IRIs each conversion, so
re-ingesting a model added new nodes instead of matching the existing ones. It is
now derived (UUIDv5) from the object's GlobalId and the set name, so it is stable
across runs, machines and releases, and distinct per (object, set).

## [0.6.1]

### Fixed — element IRIs collided (**breaking**)

Two different objects could share one RDF resource. IFC GlobalIds are base64 over
a 64-letter alphabet that contains **both** `_` and `$`, and the IRI builder
rewrote `$` to `_` — not an escape but a collision. `…TZzX$` and `…TZzX_` are two
different walls and both became `…TZzX_`, so one node ended up carrying two
walls' quantity sets, two geometries and two containments.

Measured over the corpus, this silently fused **527 objects in Atlas**, 56 in
model A and 24 in model C. It surfaced as a wall appearing to have two
identically-named quantity sets, but the duplicate sets were the symptom: the
node was two walls.

`$` and anything else outside `[A-Za-z0-9_]` is now percent-escaped, which is
injective (`%` cannot occur in a GlobalId) and decodes back to the real
identifier by the ordinary rule. The Turtle writer accepts `%HH` in a prefixed
name, as the grammar's `PLX` production allows, so output stays compact.

**Breaking:** any element whose GlobalId contains `$` changes IRI, e.g.
`inst:wall_2O2Fr_t4X7Zf8NOew3FNtn` → `inst:wall_2O2Fr%24t4X7Zf8NOew3FNtn`.
Consumers must re-ingest. The geometry producer stamps the same IRI onto its 3D
objects and shares the one implementation, so viewer links stay consistent.

### Fixed — a window wider than it is tall reported the wrong `Width`

The lining width was taken as the opening profile's *smaller* span, which is only
the width for an opening taller than it is wide. Every window in the validation
corpus is (174 of 174), so the corpus could not see it; a 2.0 × 1.6 m window
would have reported 1.6 as its width, and `GrossArea` with it. The opening's
height is already known independently from its vertical extent, so the width is
now the span that is not the height — no threshold, and identical behaviour for
the tall case.

## [0.6.0]

### Changed — QTO rebuild

The QTO module computed quantities nothing verified. Measured against the
quantities already authored in real models — a free ground-truth corpus, since
the converter never modifies them — it attempted 58.5% of them and got 37.5% of
those right.

It now attempts **45.4%** and gets **97.7%** of those right. Coverage fell and
then rose again, and both moves were deliberate: everything that could not be
measured exactly was withdrawn first, and the ground regained since is ground
that can be defended value by value.

| quantity | coverage | correct |
| --- | ---: | ---: |
| Length | 34.2% | **100.0%** |
| Perimeter | 96.4% | **100.0%** |
| GrossSurfaceArea | 69.0% | **100.0%** |
| GrossFootprintArea | 39.1% | **100.0%** |
| GrossArea | 88.2% | 99.9% |
| Width | 59.9% | 99.9% |
| NetArea | 99.1% | 99.5% |
| Height | 58.9% | 97.6% |
| GrossSideArea | 15.0% | 97.0% |
| NetVolume | 60.3% | 96.7% |
| NetSurfaceArea | 99.1% | 95.9% |
| Depth | 37.0% | 94.9% |
| GrossVolume | 93.0% | 92.5% |

An element whose geometry cannot be measured exactly yields no quantity, because
a wrong number is worse than a missing one for a consumer that calculates with
it. `Area`, `CrossSectionArea`, `OuterSurfaceArea`, `NetSideArea`, `Volume` and
the weights are computed by nothing and emitted by nothing — see below.

- **Tessellation is the primary measurement.** `ifc-lite` already evaluates every
  representation kind — sweeps, breps, booleans, half-space clips, CSG,
  revolutions, mapped items — into triangles with openings cut, so measuring
  *that* replaces a special case per solid kind. This is what IfcOpenShell does
  and why it reaches the coverage it does. Head to head on the same files,
  tessellation reached 40.4% coverage where per-representation arithmetic
  reached 25.1%.
- **Tessellated volumes are bounded before they are believed.** A shell built
  from a long boolean chain can be non-manifold and integrate to several times
  its true volume with no local sign of trouble: one ArchiCAD model returned
  6× for its median element while its surface areas stayed exact. A solid lies
  inside the prism formed by its own shadow and its own extent along the same
  axis, so a volume exceeding that product on any axis is refused. The test is a
  geometric consequence, not a tolerance — no correct volume can fail it.
- **Tessellated volumes are not used on voided elements at all.** The mesh does
  have its openings cut, but *how* cannot be checked: 56% correct on elements
  with declared voids against 96% without.
- **Dimensions come from an oriented plan rectangle**, not from world-axis
  extents. A wall at 30° has an axis-aligned box larger than itself in both plan
  directions, which is why every dimension quantity previously had to be
  withheld. The minimum-area enclosing rectangle (convex hull + rotating
  calipers) carries the orientation the object actually has. `Perimeter` went
  from 0.9% coverage to 96.4%, `Width` from 15.5% to 59.9%.
- **`stream_meshes` returns metres whatever the file declares** — it feeds a
  renderer — and the module was treating its output as raw geometry units. Every
  mesh-derived quantity in a millimetre model was wrong by 10³, 10⁶ or 10⁹.
- **A model with contradictory unit declarations is refused outright.** Merged
  files carry one `IfcUnitAssignment` per source and those can disagree; one
  corpus model declares both metres and millimetres. Iteration order over them is
  undefined, so the whole file's quantities came out either right or 10⁶ too
  small *depending on the run*. An ambiguous scale is not a scale.
- **Doors and windows are measured by the opening they fill**, which is what
  IFC's `Qto_DoorBaseQuantities` and `Qto_WindowBaseQuantities` are defined
  against — the lining, not the leaf. The nominal `OverallHeight` attribute is
  not always that: one model carries 2.35 m windows whose opening, and whose
  authored `Height`, are both 1.5 m. `Height` 85.9% → 95.6%, `Width` → 100%.
- **The opening-subtraction heuristic is gone.** `NetVolume` for a voided
  extrusion used to sum each opening's own solid with its depth capped at the
  host's thickness — a guess twice over, since an opening that stops short is
  over-subtracted and two that overlap are subtracted twice.
- **All solids in a Body are summed**, for polyhedra as well as extrusions. A
  body routinely holds several face sets — one per material layer, one per stair
  tread — and measuring only the first reported 1% of a stair flight's volume.
- **Units are resolved.** Geometry is in `LENGTHUNIT` while quantities are in the
  separately declared `AREAUNIT`/`VOLUMEUNIT`, and half the corpus mixes
  millimetre geometry with SI quantities. 45.1% of everything computed was
  emitted unconverted. Models whose unit scale cannot be established
  (conversion-based/imperial) emit nothing.
- **Polyhedra are exact.** The divergence theorem is exact for a closed
  polyhedron; the previous code fan-triangulated concave faces and discarded
  inner bounds, over-reporting breps by a median 7.24%.
- **Extrusions are exact**, and the extrusion *direction* is finally read.
- **The bounding-box tier is gone**, in both places it lived. It unioned points
  from unrelated coordinate frames and produced a 0.05 m "Depth" for a 0.3 m
  slab, read off a cutting plane's origin.
- **Every quantity was audited against its bSDD definition.** Five were computed
  correctly and labelled wrongly.

#### What is deliberately not emitted

These are refusals with a measurement behind them, not gaps:

- **`Area` on doors and windows.** Three exporters in the corpus mean three
  different things by it. Two give the lining rectangle, which the module
  reproduces exactly for all 236 of them; the third gives half the door leaf's
  total surface — 2.045 where the lining is 2.000, for 164 doors. The best single
  rule lands at 61.6%, and a value that disagrees with the file's own convention
  two times in five is worse than none. `GrossArea` carries the lining figure for
  the classes whose quantity set defines it.
- **`CrossSectionArea` and `OuterSurfaceArea`** — 0.6% and 1.6% correct when they
  were shipped in 0.5.0, now off.
- **`NetSideArea`** — the elevation is against the element's own middle plane; a
  world-axis shadow is that only for an axis-aligned wall (67.5%, p95 203%), and
  volume-over-thickness is no better (72.5%).
- **Weights** — no geometry gives a density.

#### Residual disagreement

Of the values still emitted, 2.3% disagree with the file. That residue was
decomposed rather than left as a number: the largest block is 62 columns in one
Revit export whose own quantities are in **square feet** (its `CrossSectionArea`
reads 1.07639 for a 0.25 × 0.40 m column, and its `GrossVolume` is that ft²
figure multiplied by a metre length), and the next is 58 multi-layer walls where
the file reports a nominal length × thickness × height rather than the sum of the
layer sweeps. In both the computed value is the true measurement of the authored
solid.

### Added

- `crates/qto-validate` — scores QTO output against the quantities already in a
  file, reporting coverage and accuracy separately, by quantity kind and
  representation type.
- `crates/qto-geometry/src/plan_obb.rs` — minimum-area enclosing rectangle of a
  plan projection, by convex hull and rotating calipers.
- `scripts/compare_ifcopenshell.py` — cross-checks against IfcOpenShell, which
  distinguishes "our maths is wrong" from "the authored value means something
  else". It found a real defect the authored oracle could not.

### Removed

- **The OpenCASCADE backend and the `cadrum` dependency.** It was added for
  booleans, half-space clipping and circular sweeps; the tessellated path covers
  all three, and across the corpus OCCT was asked for a measurement zero times.
  With it go the `occt` feature on three crates, the `verify-qto` workflow that
  existed to build it, and the `wasi-sdk` layer in the WASM image that existed to
  compile its C++ shim for the web target — the image no longer needs a C++
  toolchain or CMake.
- `QtoOptions`' two fields. They existed to score the tessellation approach
  against per-representation arithmetic on the same files; that comparison is
  settled, so there is nothing left to switch between and the struct is now a
  bare activation marker.

### Changed — vocabulary (**breaking**)

Every type and predicate the converter emitted is now one some resolvable
vocabulary actually describes. The terms below resolved to nothing: the triples
loaded and the counts looked right, but `rdfs:subClassOf*` found no ancestors,
SHACL could not target them, and a UI rendered a raw IRI. Consumers must update
queries and re-ingest existing models.

- `beo:{Element}-NOTDEFINED` → the BEO base class (e.g. `beo:Railing-NOTDEFINED`
  → `beo:Railing`). BEO ships the real predefined-type variants but not
  `NOTDEFINED`, which states that no subtype was given — that is the base class.
  Guarded generally against BEO's declared classes rather than special-casing
  `NOTDEFINED`, so `USERDEFINED` and enums misread from an unrelated attribute
  slot are suppressed the same way.
- `furn:Furniture` → removed. `http://pi.pauwel.be/voc/furniture#` is a dead
  host and BEO has no furniture class, so furnishing elements
  (`IFCFURNISHINGELEMENT`, `IFCFURNITURE`, `IFCSYSTEMFURNITUREELEMENT`) now carry
  `bot:Element` plus their ifcOWL / bSDD typing and no product class. This also
  fixes `IFCFURNITURE` emitting an undeclared `beo:Furniture`.
- `smls:unit` → `qudt:unit` (`http://qudt.org/schema/qudt/unit`).
  `https://w3id.org/def/smls-owl#` returns 404. Objects are unchanged — still
  unit individuals from `http://qudt.org/vocab/unit/`.
- `lbd:Project` → `dicp:ConstructionProject`
  (`https://w3id.org/digitalconstruction/0.5/Processes#`). The root node of every
  converted model was typed from a namespace with no vocabulary document behind it.
- `bot:hasSite` → `bot:containsZone`. BOT defines no `hasSite` property; `bot:Site`
  is a `bot:Zone` and zone containment is `bot:containsZone`.

Turtle prefix header: `furn:` and `smls:` removed, `qudt:` and `dicp:` added.

Non-ASCII literals are deliberately **unchanged** — the serializer emits
spec-conformant UTF-8 per RDF 1.1 N-Quads §3. Consumers that mangle umlauts are
reading the file with the wrong charset; fix that at the reader (for a Java bulk
loader, `-Dfile.encoding=UTF-8` at JVM launch, or JDK 18+).

### Added

- `ontologies/beo.ttl` — vendored Building Element Ontology v0.1.0 (CC BY 1.0,
  Pieter Pauwels) for provenance, plus `scripts/build_beo_index.py` which
  generates the embedded allowlist of BEO's declared classes
- AGPL-3.0-only license with commercial dual-license option
- GitHub Actions CI for building CLI binaries (Linux, macOS, Windows)
- Electron desktop app with native CLI sidecar (macOS + Windows)
- Per-module stage events in CLI output (timing, triple counts, success/failure)
- GitHub Releases for distributing CLI binaries and desktop installers
- Web UI download buttons point to GitHub Releases (always latest version)

### Changed

- License: MPL-2.0 / Apache-2.0 → AGPL-3.0-only (vendored geometry stays MPL-2.0)
- Download buttons: local placeholder files → GitHub Releases URLs
- `deploy-web.yml` triggers on tag push (`v*`) in addition to `main` branch push

### Removed

- Internal plan/TODO docs (geometry, owl, plugins, structured data)
- Stale artifacts: `libnull.rlib`, `ldac_clean.svg`, `e2e-test.js`, `Dockerfile.e2e`
- Superseded `scripts/build_linux_cli.sh` (replaced by `build_all_cli.sh`)
- Placeholder CLI binaries from `web/wasm-prototype/public/bin/`

## [0.1.0] — Initial Release

### Features

- IFC STEP file parser (IFC2X3, IFC4, IFC4x3)
- LBD triple producers: BOT, BEO, Props/OPM, bSDD, OMG/FOG, IfcOWL
- Geometry pipeline: tessellation, Fragments/glTF/Parquet sidecars
- RML mapper for structured data (JSON/CSV/XML)
- Ontology mapper for external ontology alignment
- OWL reasoner
- Turtle and N-Quads (including chunked) serializers
- Plugin system with preprocess, produce, postprocess, serialize, export stages
- WebAssembly web UI with real-time pipeline visualization
- CLI with explicit module selection and configuration
- Multi-threaded conversion via rayon
