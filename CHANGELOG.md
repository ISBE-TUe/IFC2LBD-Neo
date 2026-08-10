# Changelog

All notable changes to IFC2LBD-Neo are documented in this file.

## [Unreleased]

### Changed — QTO rebuild

The QTO module computed quantities nothing verified. Measured against the
quantities already authored in six real models — 89,346 standard IFC quantities
— it attempted 58.5% of them and got 37.5% of those right.

It now computes 39.1% and gets **62.5%** right, and the values it does emit are
correct far more often: `Length` 99.6%, `GrossArea` 100%, `Depth` 100%,
`Perimeter` 99.5%, `GrossFootprintArea` 99.5%, `Width` 98.3%, `GrossVolume`
93.8%. Coverage fell deliberately: an element whose geometry cannot be measured
exactly now yields no quantity, because a wrong number is worse than a missing
one for a consumer that calculates with it.

- **Units are resolved.** Geometry is in `LENGTHUNIT` while quantities are in the
  separately declared `AREAUNIT`/`VOLUMEUNIT`, and three of six corpus models mix
  millimetre geometry with SI quantities. 45.1% of everything computed was
  emitted unconverted — 10⁶ too large for areas, 10⁹ for volumes. Models whose
  unit scale cannot be established (conversion-based/imperial) now emit nothing.
- **Polyhedra are exact.** The divergence theorem is exact for a closed
  polyhedron; the previous code fan-triangulated concave faces and discarded
  inner bounds, over-reporting breps by a median 7.24%.
- **Extrusions are exact**, and the extrusion *direction* is finally read.
- **The bounding-box tier is gone.** It unioned points from unrelated coordinate
  frames and produced a 0.05 m "Depth" for a 0.3 m slab, read off a cutting
  plane's origin.
- **Every quantity was audited against its bSDD definition.** Five were computed
  correctly and labelled wrongly.
- **All solids in a Body representation are summed**, not just the first.
- **OpenCASCADE backend** behind the off-by-default `occt` feature, for booleans,
  half-space clipping and circular sweeps.

### Added

- `crates/qto-validate` — scores QTO output against the quantities already in a
  file, reporting coverage and accuracy separately, by quantity kind and
  representation type.
- `scripts/compare_ifcopenshell.py` — cross-checks against IfcOpenShell, which
  distinguishes "our maths is wrong" from "the authored value means something
  else". It found a real defect the authored oracle could not.
- `.github/workflows/verify-qto.yml` — builds the `occt` feature on Linux, macOS,
  Windows and wasm32, and asserts cadrum stays out of the default graph.
- `wasi-sdk` in the WASM image, so the same geometry backend builds for the web
  target.

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
