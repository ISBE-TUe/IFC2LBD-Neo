# Plan — geometry-driven QTO rebuild

Replace the QTO module with a geometry-driven implementation whose values are
exact, including opening subtractions and non-trivial forms.

Status: **research + design. Nothing implemented.**

---

## 0. The governing rule

> **Never write a wrong or imprecise QTO value. If it cannot be computed exactly,
> write nothing for it.**

Omission is always the correct fallback. An approximation never is. Everything
below is arranged around making that rule cheap to obey and impossible to violate
silently.

Three consequences:

- **Partial coverage is safe.** There is no big-bang replacement. Ship the
  representations that can be computed exactly, emit nothing for the rest, widen
  over time.
- **CLI and WASM may differ in coverage, never in correctness.** A unified backend
  is the goal (§4); a CLI superset is an accepted fallback outcome (§5.4). What is
  not acceptable is either target emitting a value the other would contradict.
- **Existing authored data is never replaced.** Already true of the current module,
  and the one part worth keeping wholesale.

---

## 1. Why replace rather than repair

Measured by running the current plugin on known-answer geometry:

| input | expected | actual |
| --- | --- | --- |
| Slab 6×4×0.3, boolean clip, half-space as **first** operand | Depth 0.3, Vol 7.2 | **Depth 0.05, Vol 1.2** |
| Slab 6×4×0.3, `IFCUSHAPEPROFILEDEF` | full set | **no quantity set at all** |
| Space 4×3×2.7 | Height 2.7, GrossFloorArea 12 | **only GrossVolume / NetVolume** |
| Proxy brep 2×1×4 | — | **only GrossVolume** |
| Wall 5.0×0.2×3.0 | Length 5, Width 0.2, GrossSideArea 15 | **no Length, no Width, GrossSideArea 31.2** |

The 0.05 depth came from a *clipping plane's* placement origin. `bbox::compute`
unions every `IFCCARTESIANPOINT` reachable from every shape representation — profile
-local 2D points, placement origins, axis curves, cutting planes — with no transform
and no filtering. That is not a bug to fix; the approach has no valid form.

Compounding: booleans are followed only through `args[1]`; 9 of ~20 profile types are
implemented; the type→quantity table is 14 hand-written entries with a
single-quantity `_` fallback; `NetArea`/`NetSideArea` are never computed at all;
`GrossSideArea` uses the extrusion's full lateral wrap; the extrusion **direction** is
never read. And `inject` drops non-positive values, so every failure becomes a silent
omission rather than a logged one.

---

## 2. What "exact" can and cannot mean

**Exactly computable:**

- Volume, cross-section area and perimeter of an extrusion with an analytic or
  polygonal profile — `area × depth`, closed form.
- Openings that are extrusions along the same axis — subtract in **2D on the
  profile**, then extrude. Exact, and immune to every mesh-CSG failure mode.
- Any polyhedral solid (`IfcFacetedBrep`, `IfcTriangulatedFaceSet`,
  `IfcPolygonalFaceSet`) — the divergence-theorem sum is exact for polyhedra.

**Not computable from a mesh, at any refinement:**

- Curved surfaces — cylinders, revolutions, swept disks, NURBS
  (`IfcAdvancedBrep`). Tessellation always under-estimates a convex curved volume.
  Refinement shrinks the error; it never removes it.

The only route to exactness for curved geometry is a kernel that integrates over
the **actual surfaces** rather than facets. That is what makes OCCT a candidate
rather than a luxury (§4.C).

Under §0 the alternative to exact is not "approximate and label" — it is **emit
nothing, and record why**. Provenance exists to make coverage gaps visible, not to
license bad numbers.

---

## 3. The oracle — how precision gets proven

**The quantities already in the IFC files are the test set.**

Real models ship quantities computed by the authoring tool's own kernel. The rule
"never replace authored data" means we never touch them — which makes them a free,
large ground-truth corpus:

```
for every element that ALREADY HAS an authored quantity:
    compute it anyway, in a harness (never in the pipeline)
    compare computed vs authored
    report error distribution per quantity kind × representation type × backend
```

This is what turns every question below from a judgement call into a measurement,
on real models, before a single computed value is trusted for the elements that
lack them. It also catches unit-scaling errors immediately.

**Build this first.** It is the acceptance test for everything else and the
scoreboard for the bake-off in §4. It needs no ground truth that isn't already on
disk.

Two caveats worth stating up front: authored quantities are not themselves
infallible (exporters disagree, and some are stale relative to the geometry), and
conventions differ — so the harness reports *distributions and outliers*, and
disagreement is a prompt to investigate rather than an automatic failure.

---

## 3a. P0 results — the measured baseline

`crates/qto-validate` is built and run over **the full available corpus** — 8 real
models, of which 6 are scoreable (§3c). **89,346 standard IFC quantities** after
excluding 138,245 exporter-specific ones (§3b).

| | |
| --- | ---: |
| coverage (attempted at all) | **58.5%** |
| of those computed, match within 0.1% | **37.5%** |
| **⇒ share of emitted values that are wrong** | **~62%** |
| emitted in raw geometry units (unusable as written) | **45.1%** |

Per quantity kind — the defects predicted from code review, now quantified:

| quantity | n | coverage | match | median err | |
| --- | ---: | ---: | ---: | ---: | --- |
| `OuterSurfaceArea` | 14,515 | 79.8% | **1.8%** | 99.8% | ~2× lateral-wrap, on the 3rd-largest quantity |
| `CrossSectionArea` | 3,147 | 69.2% | **0.6%** | 97% | wrong nearly every time it is emitted |
| `GrossSideArea` | 441 | 63.7% | **14.6%** | 93% | same 2× family |
| `NetVolume` | 19,520 | 78.5% | **28.1%** | **2.62%** | a systematic *bias*, not a tail — see below |
| `Length` | 17,223 | 82.4% | 56.6% | 0 | p95 196% |
| `GrossVolume` | 2,812 | 99.0% | 58.7% | 0 | |
| `Width` | 3,104 | 34.4% | 91.4% | 0 | **p95 2460%** — wall thickness/length swap |
| `NetWeight`, `NetSurfaceArea`, `NetArea`, `NetSideArea`, `GrossWeight`, `GrossSurfaceArea`, `Volume`, `NetFootprintArea`, `TotalSurfaceArea`, `ProjectedArea` | 22,924 | **0%** | — | — | no tier computes any of these |
| `Perimeter` | 1,524 | 100% | **96.9%** | 0 | correct |
| `GrossArea` | 1,531 | 88.6% | **96.0%** | 0 | correct |

By representation:

| representation | n | coverage | match | median err |
| --- | ---: | ---: | ---: | ---: |
| faceted-brep | 51,028 | 51.5% | 27.2% | 7.33% |
| extruded/analytic-profile | 20,580 | 75.0% | 34.4% | 2.40% |
| extruded/arbitrary-profile | 12,798 | 71.0% | 66.4% | 0 |
| tessellated | 3,994 | 35.9% | 77.4% | 0 |
| boolean | 32 | 75.0% | 33.3% | 43.7% |

### `NetVolume`'s systematic bias — diagnosed

Not a tail: across 15,319 computed values the computed figure is **higher in 71.5%
of cases and lower in 0.6%**. A one-directional over-estimate.

| | signed median | % higher | n |
| --- | ---: | ---: | ---: |
| faceted-brep | **+7.24%** | 77.2% | 7,638 |
| extruded/analytic-profile | **+2.62%** | 98.1% | 5,143 |
| extruded/arbitrary-profile | −0.00% | 0.0% | 2,523 |
| boolean | +38.38% | 100% | 8 |

By type: `IFCBEAM` +7.24%, `IFCCOLUMN` +2.62%; `IFCWALL`, `IFCSLAB`, `IFCPLATE`
and `IFCFOOTING` are exact.

**Cause, traced to a specific element** (`IFCBEAM 2sFYqIn3r2pAROZO2jXagz`, a
`IFCFACETEDBREP` reinforcement anchor, authored 0.000212 m³ vs computed 0.000228,
+7.50%). Its closed shell has 68 faces with **4 inner bounds** and loops of 4, 12
and 20 vertices. Two independent defects in `mesh_volume::tessellate_face`, both
inflating volume:

1. **Inner bounds are discarded.** The loop does `break; // outer bound only`
   after the first bound, so holes in a face are never subtracted.
2. **Fan triangulation from vertex 0** is only valid for convex polygons. The 12-
   and 20-vertex faces here are concave, so the fan produces overlapping and
   inverted triangles and the divergence-theorem sum is wrong.

Both are fixable without a new kernel: the vendored ifc-lite already exposes
`triangulate_polygon` (earcut) and handles profiles with holes. Until then,
faceted-brep volumes are knowingly over-estimated and — under §0 — should be
withheld rather than emitted.

A quantity that is *quietly 2–7% wrong everywhere* is more dangerous downstream
than one that is obviously broken, because nothing flags it.

> **Earlier 4-model figures (46.1% coverage / 76.1% match / ~24% wrong) are
> superseded.** The larger models are substantially worse, and the small-model
> sample was optimistic by a wide margin. Any figure quoted must name its corpus.

**The error distribution is bimodal** — median 0 with a catastrophic tail — so spot
-checking a handful of elements would have looked fine. That is how this survived.

### 3c. The corpus

| model | size | elements | standard qty | coverage | length unit |
| --- | ---: | ---: | ---: | ---: | --- |
| model A | 8.6 MB | 957 | 2,917 | 73.1% | m |
| model B | 13 MB | 3,807 | 11,392 | 70.8% | m |
| model C | 24 MB | 6,623 | 13,052 | 9.9% | **mm** |
| model D | 58 MB | 2,986 | 12,386 | 55.0% | m |
| model E-…-820100_WIP | 69 MB | 35,359 | 47,116 | 71.4% | **mm** |
| model H | 192 MB | 4,494 | 2,483 | 12.8% | **mm** |
| 20210219Architecture | 108 MB | — | — | **skipped** | **FOOT** |
| I90_BBH_A6_B70_… | 314 MB | — | — | **skipped** | m |

Performance is not a concern: 2.15 s and 3.4 GB peak RSS on the 314 MB file.

Two models cannot be scored, and both are informative:

- **`20210219Architecture` is imperial** — `LENGTHUNIT` is a conversion-based unit
  (FOOT). The harness refuses rather than scoring against an unknown scale. **The
  production module has no conversion-based unit handling either**, so it is
  silently wrong on such files. Resolving `IfcMeasureWithUnit` conversion factors
  is required for both.
- **`I90_BBH_A6_…` (infrastructure, 314 MB) has no authored quantities at all.**
  Nothing to validate against — and it is precisely the kind of model where a QTO
  rebuild matters most, because there is no authored data to fall back on. The
  oracle in §3 cannot cover this class; correctness there must come from §5.3
  cross-backend agreement instead.

### Findings that change the plan

1. **No unit handling anywhere, and it affects most of the corpus.** **Three of the
   six scoreable models are millimetre** (model C, model E, 2906) while declaring
   `AREAUNIT` = m² and `VOLUMEUNIT` = m³ — legal, common, and the norm in
   German/Dutch exports. The module emits raw geometry-unit numbers: **10⁶ too
   large for areas, 10⁹ for volumes**. **45.1% of every value it computes across
   the corpus is affected** (67.5% on model E, 66.7% on model C, 58.8% on 2906).
   Unit resolution is a **first-class requirement of Layer 2**, not an afterthought
   — and it must cover conversion-based (imperial) units too.
2. **Geometry errors were hiding behind unit errors.** Before normalising, faceted
   -brep volumes showed ~10¹¹% error; afterwards the same elements show a median of
   0 with a real ~2× tail. Any accuracy number quoted without unit normalisation is
   meaningless.
3. **Real models use `GrossFootprintArea`** (lowercase p) — 504 instances in the
   corpus — while bSDD IFC4x3 lists `GrossFootPrintArea`. **This resolves §8.3:
   emit the IFC4 lowercase-p spelling**, or the audit's name match misses and the
   quantity is duplicated rather than filled. The harness matches
   case-insensitively for exactly this reason.

### 3a-bis. Cross-checked against IfcOpenShell

The authored-quantity oracle cannot say who is wrong when we disagree with it.
IfcOpenShell — an independent implementation on OCCT via IfcGeom — can.
`scripts/compare_ifcopenshell.py` runs the three-way comparison.

`NetVolume`, elements where both produce a value:

| model | IfcOpenShell vs authored | **ours** vs authored | ours vs **IfcOpenShell** |
| --- | ---: | ---: | ---: |
| model A | 79.6% | 77.8% | 78.4% |
| Atlas | 100.0% | 99.9% | **99.9%** |
| model C | 100.0% | 100.0% | **100.0%** |
| 2906 | 96.4% | 96.4% | **100.0%** |
| model E (4,000 sampled) | 58.2% | 58.2% | 58.2% |

Two conclusions, both load-bearing:

**Most disagreement with authored data is not ours.** Where IfcOpenShell scores
79.6% or 58.2% against the authored values, we score 77.8% and 58.2%. A mature
reference implementation disagrees with those files to the same degree, so the
gap is in the files, not in the arithmetic.

**Where we differ from IfcOpenShell, we are the more exact.** On model E only 446
of 4,000 differ, almost all `IfcColumn` with `CHS` circular hollow sections, and
the direction is diagnostic:

```
ours / IfcOpenShell = 1.0098      we report 0.98% more
ours / authored     = 1.0262
IfcOpenShell / auth = 1.0162
```

IfcOpenShell tessellates the curved section, so its inscribed polygon
under-reports by ~1%. The analytic path computes `π(R²−r²)·L` in closed form.
This is precisely the class of case §2 predicts: tessellation cannot reproduce a
curved volume, and here it measurably does not.

Two elements in Atlas show the opposite pattern — `ours/authored` 0.197 while
`IfcOpenShell/authored` is exactly 1.000. That is a genuine defect on our side
and the correct use of this harness: a disagreement with authored data that
IfcOpenShell does *not* share is ours to fix.

### 3b. Methodological note: vendor quantities must be excluded

The model D model contributes 150,165 authored quantities of which **zero** are
standard — they are ArchiCAD sets (`ArchiCADQuantities`, `AC_Equantity_*`) holding
things like `Oberkante zu Meereshöhe` and `Höhe zu 1. Referenzhöhe`. Scoring
against those drove apparent coverage to 10.3% and meant nothing: no geometry
backend should compute them.

Filtering by *set* name does not work — that same model puts genuine base
quantities in ArchiCAD's `BaseQuantities`, not a `Qto_*` set. The harness therefore
filters by **quantity name**, against the 55 standard names extracted from the
vendored bSDD index, case-insensitively.

Any future coverage figure must state whether it is over standard quantities only.

---

## 4. Candidate backends — the bake-off

The goal is one backend that behaves identically on CLI and WASM. Four candidates,
all implementing the same interface, all scored on the same corpus by §3's harness.

```rust
// The only contract. Backends are interchangeable behind it.
fn compute(element: &Element) -> Vec<(QuantityKind, f64, Provenance)>
// Absence of a kind == "could not be computed exactly". Never a guess.
```

### A. Analytic, pure Rust — *unified*

Extrusion + profile mathematics in f64, with **opening subtraction done in 2D** on
the profile via `bool2d` / `i_overlay` (both already in the dependency graph).
Reads the extrusion direction, so height/thickness/length are assigned correctly.

- Exact for everything it covers; no new dependency; identical on both targets.
- Cannot touch curved solids, advanced B-reps, or booleans that aren't
  profile-expressible.
- **Lowest risk, and useful regardless of which candidate wins** — it is the fast
  path even if OCCT lands.

### B. f64 polyhedral mesh — *unified*

Drive the vendored ifc-lite processors but keep vertices in **f64** and compute the
divergence-theorem sum, with a watertightness check that withholds a value rather
than returning a wrong one.

- Exact for polyhedra. Covers tessellated and faceted-brep geometry that A cannot.
- Requires an f64 path: the vendored `Mesh` stores `Vec<f32>` (`mesh.rs:56`) and
  `filter_stretched_triangles` deletes triangles, breaking watertightness. Either a
  parallel f64 mesh type in the vendored crate, or compute quantities inside the
  processors before the f32 downcast. Carries a diff against upstream
  `LTplus-AG/ifc-lite` either way — worth asking whether they'd take it.
- Still not exact for curved surfaces (it is tessellation).

### C. OCCT via `cadrum` — *potentially unified, and exact for curves*

This is the candidate that changed the shape of the plan.

`cadrum` ships **prebuilt static OCCT 8.0.0**, downloaded by `cargo build`, with no
C++ toolchain required, for:

| target | prebuilt |
| --- | --- |
| `x86_64-unknown-linux-gnu` | ✅ |
| `aarch64-unknown-linux-gnu` | ✅ |
| `x86_64-pc-windows-msvc` / `-gnu` | ✅ |
| `aarch64-apple-darwin` / `x86_64-apple-darwin` | ✅ |
| **`wasm32-unknown-unknown`** | ✅ (Docker build) |

That covers every CLI target this repo releases **and the exact WASM target it
builds** — and this repo already has the Docker WASM build (`docker-compose.yml`,
`scripts/build_wasm_web.sh`). There is a live in-browser STEP→glTF demo.

It exposes precisely what quantities need: `Solid::volume`, `Solid::area`,
`Solid::center`, `Solid::inertia`, `Solid::bounding_box`, `Solid::contains`, plus
booleans, extrude, sweep, loft. OCCT's volume/area come from Gauss integration over
the real surfaces — **exact for planes, cylinders, cones, spheres, tori and NURBS**,
and its booleans report failure explicitly, which is exactly the signal §0 needs.

Risks to establish in the spike, not to assume away:

- **Maturity.** v0.8.16, ~2.2k downloads. Young. API churn is likely.
- **Expressiveness.** IFC profiles (arbitrary polylines with arcs, I/L/T/U/Z shapes,
  profiles with voids) must be constructible through its wire/face API. Probably
  yes; unverified.
- **Size.** OCCT in WASM is multi-megabyte (a comparable build is ~4.5 MB brotli).
  Material for the browser prototype, irrelevant for CLI.
- **Determinism across targets.** The single most important thing to measure — see
  §5.2.

### C — P4 spike results (run)

**Native: decisive pass.** A standalone spike built against `cadrum = "0.8"` on
`aarch64-apple-darwin`. Build took **14 seconds** — the prebuilt OCCT is
downloaded and linked, no CMake, no C++ toolchain setup, no source build.

Every quantity came back exact to f64 round-off:

| test | result | rel. error |
| --- | --- | ---: |
| cube 5×4×3 volume and surface area | EXACT | 0 |
| **cylinder r=3 h=10 volume** | **EXACT** | **0** |
| cylinder surface area | EXACT | 1.2e-16 |
| concave L-profile extruded (area 6 × depth 12) | EXACT | 0 |
| wall minus opening (boolean subtract) | EXACT | 1.7e-16 |
| 10×10×2 plate minus r=2 bore | EXACT | 3.3e-16 |
| oblique extrusion (direction not ⟂ to profile) | EXACT | 0 |

The cylinder is the one that matters: **no tessellating backend can produce
`πr²h` to round-off**, and Track B never will. The concave-L and the bored plate
are precisely the two cases `mesh_volume` gets wrong (§3a) — both exact here.

The API maps onto IFC directly: `Edge::polygon` for
`IfcArbitraryClosedProfileDef`, `Edge::circle` for `IfcCircleProfileDef`,
`Solid::extrude(profile, direction)` taking a **direction vector** (so IFC's
extrusion direction is native, fixing §1.7), `&a - &b` for voids, and
`Solid::{volume, area, bounding_box, center, inertia}` for the quantities.

One caveat found: `bounding_box` is inflated by OCCT's tolerance (~1e-7 per
axis). Harmless, but the Layer 3 gate must not treat it as exact.

**WASM: also a pass — and bit-identical to native.**

A plain host-toolchain build fails (Apple's `clang++` has no WebAssembly backend;
Homebrew LLVM has the backend but there is no wasm C++ sysroot/libc++ locally).
That is expected: the `cxx` bridge compiles C++ shim code for the target even
though the OCCT itself is prebuilt. cadrum ships the toolchain as a container,
`ghcr.io/lzpel/cross-wasm32-unknown-unknown`, and with it:

```sh
docker run --rm --platform linux/amd64 -v "$PWD":/work -w /work \
  ghcr.io/lzpel/cross-wasm32-unknown-unknown cargo build --release
```

builds in **41 s**, producing a 10 MB `.wasm` (**3.2 MB gzipped**).

Running the identical computations under Node and diffing against the native
aarch64 build:

| probe | native + wasm (identical) |
| --- | --- |
| cube 5×4×3 volume | 60 |
| cylinder r=3 h=10 volume | 282.74333882308139 |
| cylinder r=3 h=10 area | 245.04422698000388 |
| L-profile extruded volume | 72 |
| wall minus opening | 2.5999999999999996 |
| plate minus bore | 174.8672587712816 |

**Bit-identical across targets.** This is the §5.2 gate, and it passes — in sharp
contrast to the existing Manifold path, which is documented in `csg.rs` as
producing different results on Linux and macOS for the same input.

Two requirements the integration must honour:

- **`__wasm_call_ctors()` must be called once before any cadrum call**, otherwise
  OCCT's C++ static constructors never run and the first call traps. cadrum
  exposes `__anchor_wasi_stub()` for this.
- The module uses Wasm exception handling (`-fwasm-exceptions`), so it needs a
  current browser or Node. No flags required.

Practical notes: the cross image is **amd64-only**, so Apple Silicon needs
`--platform linux/amd64` (emulated, and still 41 s). Linux and Windows *native*
builds are untested here and should be confirmed in CI.

**Consequence for §5.4: the unified outcome is available.** One backend, exact on
both targets, deterministic between them — the best case in the decision table,
not the fallback.

### C — performance, measured

14-core machine, release build:

| operation | serial | parallel speedup |
| --- | ---: | ---: |
| extrude + volume + area | 0.20 ms | **5.8×** |
| extrude + 2 boolean cuts | 3.35 ms | **1.4×** |

Serial and parallel results agree exactly. But booleans are 17× costlier *and*
barely parallelise — 1.4× on 14 cores indicates lock or allocator contention
inside OCCT. **More cores will not fix boolean-heavy models.**

`Solid` is `Send` but **not `Sync`**, which is sufficient: each element's solids
are locals and only `f64` crosses threads.

What the corpus actually contains:

| model | voids | booleans | **distinct solids** | mapped items | elements |
| --- | ---: | ---: | ---: | ---: | ---: |
| model A | 244 | 74 | 1,441 | 465 | 957 |
| Atlas | 165 | 29 | 914 | 4,001 | 3,807 |
| model C | 0 | 0 | 1,087 | 4,742 | 6,623 |
| model D | 205 | **41,771** | 3,339 | 200 | 2,986 |
| model E | 0 | 9 | 2,541 | **92,361** | 35,359 |
| 2906 | 0 | 527 | 871 | 377 | 4,494 |

**Dedup by distinct solid is the decisive optimisation, not a later tweak.**
Every model has only 871–3,339 distinct solids regardless of element count;
model E's 92,361 mapped items resolve to 2,541 solids, a 36× reuse factor.
Computing per solid turns that model from ~7 s into well under a second. Volume
is invariant under the rigid transforms mapped items apply — but the
transformation operator's **scale factor must be checked**, since a scaled
instance is not a cache hit.

model D is the worst case at 41,771 boolean nodes; budget roughly a minute
there and do not expect cores to help. A per-element time budget is the
containment: under §0, exceeding it means *omit*, which is always safe.

WASM is single-threaded, so the serial column applies — fine for extrusions with
dedup, and booleans are where it would bite.

Caveat: these timings use synthetic geometry simpler than real IFC profiles, so
treat them as optimistic lower bounds. The reuse factors are from the real files.

### C — a capability gap that shaped the design

**cadrum cannot build a solid from faces.** There is no sew/`from_faces` API;
`Solid::shell` is hollowing, and `Face` supports only iteration and projection.
So `IfcFacetedBrep` — the **largest** representation class in the corpus at
51,028 quantities — cannot be constructed through it.

This turns out to be the right split rather than a limitation:

| representation | path | why |
| --- | --- | --- |
| faceted brep, tessellated, surface model (51k) | **pure Rust, exact** | the divergence theorem is exact for polyhedra; a kernel adds nothing |
| extrusions, revolutions, sweeps, booleans, CSG (33k) | **OCCT via cadrum** | only a B-rep kernel gets `πr²h` right |

The corpus supports it: the brep-heavy models (model C, model E) have zero voids
and ~zero booleans, so "a brep that also needs a boolean" is rare. Where it does
occur, the polyhedral path refuses and the quantity is omitted.

### D. OCCT native-only via `occt-sys` — *CLI-only fallback*

`occt-sys` 0.6.0, 87k downloads, but ~1.5 years stale and it compiles OCCT through
CMake (long cold builds, C++ toolchain mandatory everywhere). Only worth pursuing if
C's WASM path fails **and** C's native path also proves unusable.

### Not pursued

`truck` (pure-Rust B-rep — the only thing that would make curves exact without C++,
but a second full geometry stack and no IFC mapping exists), `fornjot` (pre-1.0),
`csgrs` (same robustness class as the BSP already vendored), `parry3d` (mass
properties only; the tetrahedral sum is 20 lines and already written),
`occt-wasm` (TypeScript-first, npm-shaped; `cadrum` is the better Rust fit).

### The shared hard part

For C and D alike, the OCCT binding is the *easy* half. **OCCT does not read IFC.**
The IFC→OCCT mapping is what IfcOpenShell's `IfcGeom` has spent ~15 years on, and no
Rust port exists.

Two things make this tractable. §0 permits a partial mapping — map what can be
mapped exactly, emit nothing else. And the vendored ifc-lite **already parses IFC
representations into intermediates** (`Profile2D`, `Profile2DWithVoids`, extrusion
parameters, boolean trees, `VoidIndex`). The work is retargeting those intermediates
to OCCT builders instead of to meshes, one representation type at a time — far
smaller than porting `IfcGeom`.

---

## 5. How the comparison is run

### 5.1 Method

All candidates sit behind the §4 interface and run over the same corpus. The §3
harness scores each on:

| metric | why it matters |
| --- | --- |
| **Coverage** | % of elements for which the backend can emit *at least* the bSDD-required quantities exactly. The headline number. |
| **Accuracy vs authored** | error distribution against the oracle, per quantity kind × representation type. Median, p95, and the outlier list. |
| **Cross-target determinism** | same input, CLI vs WASM: **bit-identical or it does not ship** (§5.2). |
| **Cross-backend agreement** | where two backends both claim exactness, do they agree? (§5.3) |
| **Cost** | wall-clock per model; binary size; WASM bundle size. |

Report per **representation type** (extruded/analytic profile, extruded/arbitrary,
faceted brep, tessellated, boolean-clipped, mapped item, revolved, swept disk,
advanced brep), because that is the axis on which coverage decisions get made.

### 5.2 Cross-target determinism is a correctness gate, not a nice-to-have

There is precedent in this repo for exactly this going wrong. `csg.rs` documents a
*"Linux-specific Manifold pathology"* where a clipped wall collapses, noting *"macOS
aarch64 produces the full pentagon on identical input"* — a kernel-determinism bug,
guarded by heuristics, whose fallback silently returns the **un-cut host** (so
NetVolume quietly equals GrossVolume).

Under §0 that class of failure is unacceptable. So: run the corpus on Linux, macOS,
Windows and WASM, and **diff the outputs**. Any element where targets disagree is
withheld on all of them until understood. This is the single most valuable test in
the whole plan and it is cheap once §3 exists.

### 5.3 Differential testing between backends

Where A and C both claim to compute a quantity exactly, they must agree to f64
tolerance. Disagreement means at least one is wrong and **neither is trusted** for
that element. This finds mapping errors that the authored-quantity oracle cannot,
because it does not depend on the exporter having been right.

### 5.4 Decision rule, decided by the numbers

> **Decided (P5): C passes on both targets.** The spike in §4 C settled this —
> exact on native, exact on wasm32, and bit-identical between them. A single OCCT
> backend it is. Backend A is demoted from "the main path" to an optional fast
> path, to be added only if profiling shows OCCT is too slow for extrusions, and
> only where it agrees with OCCT exactly (§5.3).
>
> **What is still unproven is the IFC → OCCT mapping**, not the kernel. That is
> where the remaining risk lives — see §4's "shared hard part".

| outcome | action |
| --- | --- |
| **C passes on both targets** ← **this one** | Single OCCT backend everywhere. Best case: unified *and* exact for curves. Keep A as a fast path where it is exact and agrees. |
| **C passes natively, fails on WASM** | **Accepted split**: CLI = C, WASM = A (+B). CLI emits a superset. Coverage difference is declared in provenance and documented, never silently divergent. |
| **C too immature either way** | A (+B) on both targets. Everything else omitted. Revisit C when it matures. |
| **A alone already covers ~everything** | Ship A, skip the rest. §3 tells us whether this is the world we live in. |

The last row is a real possibility and worth taking seriously before spending
effort on C. **Measure the residual first.**

---

## 6. Architecture

Four layers. Only Layer 2 changes when the backend decision lands.

### Layer 1 — Spec: what *should* exist

Replace `qto_names.rs` (14 hand-written types, single-quantity `_` fallback) with a
table generated from the **already-vendored** bSDD index
(`crates/lbd-converter/resources/bsdd_ifc4x3_index.json.gz`), same pattern as
`scripts/build_beo_index.py`.

Its `exact` map is keyed `class|set|property` and contains **111 `Qto_*` sets across
1,351 IFC classes**. For comparison: it gives `ifcbuildingelementproxy` four
quantities (GrossVolume, NetVolume, GrossSurfaceArea, NetSurfaceArea) where the
current code emits one; `ifcspace` thirteen where the current code emits two.

Exclusions: `Qto_BodyGeometryValidation` (a validation set, not base quantities) and
`GrossWeight`/`NetWeight` (need material density, not geometry).

⚠️ **Resolve first:** bSDD IFC4x3 spells it `GrossFootPrintArea` (capital P) where
the current code writes `GrossFootprintArea`. Emitting the wrong spelling means the
audit's name match misses and the quantity is **duplicated** rather than filled.
Check against the actual schema version and against real models before emitting.

### Layer 2 — Compute: the backend, behind the §4 interface

Whatever §5.4 selects. Independently testable, swappable, and — importantly —
allowed to differ between CLI and WASM builds without any other layer noticing.

### Layer 3 — Gate: nothing implausible escapes

Every candidate value is checked before injection against an independently computed,
*trustworthy* element AABB (from the geometry pipeline's transformed mesh, never
§1's point soup):

- volume ≤ bbox volume, and above a floor fraction of it;
- each length ≤ the corresponding bbox edge;
- `NetVolume ≤ GrossVolume`; areas non-negative; no NaN/inf.

A failing value is **dropped and logged with the element GUID and the reason**. This
is the structural fix for the current module's silent-omission behaviour, and it is
the last line of defence for §0.

### Layer 4 — Inject: unchanged contract, richer output

Keep the existing audit/inject split — it already guarantees authored data is never
replaced. Add provenance per quantity:

```
method     = analytic | polyhedral | occt
exactness  = exact                      // the only value that is ever emitted
openings   = subtracted | none-present
backend    = <build target + backend id>
```

and, for what was *not* emitted, a machine-readable reason so coverage gaps are
visible rather than inferred from absence.

Emitting provenance into RDF needs a vocabulary decision (§8.4) — and given the
vocabulary work just completed, it must not become another undescribed namespace.

---

## 7. Phasing

| # | Deliverable | Notes |
| --- | --- | --- |
| **P0** | **Validation harness (§3)** — compute-and-compare against authored quantities, reporting per quantity kind × representation type | Acceptance test and bake-off scoreboard. Everything depends on it. |
| **P1** | Layer 1 — bSDD-generated spec table | Pure data, no geometry. Fixes the proxy complaint and extends 14 → 1,351 types on its own. |
| **P2** | Layer 3 gate + provenance logging, wrapped around the **existing** compute | Stops wrong values reaching output **now**, before any rewrite lands. Cheap, and independently valuable. |
| **P3** | Backend A (analytic + 2D opening subtraction), scored by P0 | Establishes the residual. May turn out to be most of the answer. |
| **P4** | **Spike C** — `cadrum` on one representation type, both targets, incl. §5.2 determinism diff | Timeboxed. Answers unified-vs-split with evidence. |
| **P5** | **Decision (§5.4)**, then build out the chosen backend | |
| **P6** | Backend B if the residual justifies it | Only if polyhedral coverage is material after A and C. |
| **P7** | Provenance into RDF | Needs §8.4. |

P0–P2 are worth doing regardless of how P4 turns out. P2 in particular should not
wait: **today the module emits wrong values into production data**, and the gate
stops that without needing the rewrite.

---

## 8. Open questions

1. **cadrum expressiveness.** Can IFC's profile zoo (arbitrary polylines with arcs,
   I/L/T/U/Z shapes, profiles with voids) be built through its wire/face API?
   First thing P4 should answer.
2. **f64 meshes (Backend B).** Parallel f64 mesh type in the vendored crate, or
   compute inside the processors before the f32 downcast? Either carries a diff
   against upstream ifc-lite.
3. **`GrossFootPrintArea` vs `GrossFootprintArea`.** Which spelling is in the real
   models? Blocks P1.
4. **Provenance vocabulary.** These are quantities on OPM property states. Existing
   term for computation method, or mint under `https://w3id.org/ifc2lbd/…` as with
   `topo:` and `bsddm:`? Must be resolvable either way.
5. **Space measurement convention.** `IfcSpace` floor area and volume are the most
   commercially loaded numbers in a model and depend on convention (to finish? to
   structure? net vs gross?). bSDD lists 13 quantities. Which does the downstream
   need, and against which convention are they validated?
6. **WASM bundle budget.** If C ships on WASM, what size increase is acceptable for
   the browser prototype? May force the split even if C works technically.

Licensing is out of scope for this plan — confirmed handled.

---

## 9. What does not change

- **Authored data is never replaced.** The audit/inject split that guarantees it
  stays.
- QTO stays a Preprocess plugin with `FailurePolicy::Optional`.
- No approximation is ever emitted, on any target, under any backend.
