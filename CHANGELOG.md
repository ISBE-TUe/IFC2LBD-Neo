# Changelog

All notable changes to IFC2LBD-Neo are documented in this file.

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
