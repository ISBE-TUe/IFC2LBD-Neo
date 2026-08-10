# Plan — vocabulary fixes from `handoff-ifc2lbd-neo-vocabulary-fixes.md`

Companion to `handoff-ifc2lbd-neo-vocabulary-fixes.md`. That document states the
problems; this one states what we will change, where, and in what order.

Status: **implemented** (W1–W7), not yet committed or run against a real model.
Handoff §6 is **not** a converter change — see §A.1.

§F.0 (should the project node exist at all?) was resolved as **Option B — keep the
node, retype it** `dicp:ConstructionProject`. That is the conservative reading: it
is what the handoff asks for, it loses no data, and it is trivially reversible into
Option A later if cn3-pt1 confirms nothing queries the project root. Option A is
not reversible without a coordinated re-ingest, so it is not a safe default to take
unilaterally. **The question in §F.0 is still worth answering.**

---

## A. Decisions taken

| Handoff § | Decision |
| --- | --- |
| §1 `-NOTDEFINED` | Vendor BEO's declared class list and guard **generally** — emit a type only when the IRI is declared. Not a `NOTDEFINED` special case. |
| §2 `smls:unit` | **Hard switch** to `qudt:unit` in one release. No dual-emit, no flag. cn3-pt1 migrates its 7 queries and re-ingests. |
| §3 `furn:Furniture` | **Drop.** Furnishing elements keep `bot:Element` (+ their ifcOWL / bSDD typing). The `furn:` namespace leaves the codebase entirely. |
| §4 `LBD#Project` | **OPEN — see §F.0.** Either drop the project node entirely (preferred if nothing queries it) or retype it `dicp:ConstructionProject`. `lbd:hasBoundingBox` and the unused `LBD#` constants stay either way. |
| §5 `bot:hasSite` | → `bot:containsZone` — **but moot if §F.0 resolves to "drop"**, since the project→site edge is the only `hasSite` use. See §F.1. |
| §6 non-ASCII escaping | **No converter change. Rejected as a converter bug.** Fixed on the cn3-pt1 side by setting the loader JVM's charset. See §A.1. |
| `schema:value` http form | **Untouched.** Explicitly out of scope per the handoff. |

### A.1 Why §6 is not a converter change

The handoff asks the serializer to escape non-ASCII as `\uXXXX`. Declined, because
the converter is not what is broken.

[RDF 1.1 N-Quads §3](https://www.w3.org/TR/n-quads/#sec-encoding) mandates UTF-8.
`lbd-serializer` writes valid UTF-8. The mangling (`Türöffnung` → `TÃ¼rÃ¶ffnung`)
is the exact signature of UTF-8 bytes decoded as Latin-1 — i.e. Blazegraph's
`DataLoader` running on a JVM whose `Charset.defaultCharset()` resolved to
ISO-8859-1, which is what happens in a container with no locale set.

Fix, applied in cn3-pt1's deployment, not here:

- set `LANG=C.UTF-8` / `-Dfile.encoding=UTF-8` **at JVM launch** — the default
  charset is cached at startup, so setting it later has no effect; or
- run the loader on JDK 18+, where [JEP 400](https://openjdk.org/jeps/400) makes
  UTF-8 the default outright.

Consequence for this repo: `escape_literal` (`lbd-serializer/src/lib.rs:491`)
stays as it is, Turtle and N-Quads output stay human-readable UTF-8, and there is
no per-character branch to pay for. The one residual risk is that the loader's
charset is deployment config and can regress silently — that is cn3-pt1's to own,
and it is why the verification in §D still checks a German literal end to end.

---

## B. What the codebase actually looks like

Verified 2026-08-10 against `main` @ `314adcb`.

### B.1 There are two conversion paths, and they are not equivalent

- **Modular / named-graph path** — `stream_bot`, `stream_beo`, `stream_props_opm`,
  `stream_bsdd_with_cache`, `stream_omg_fog`, `stream_ifcowl`, each tagged with a
  graph IRI of `{base_uri}bot|beo|props|bsdd|omg|ifcowl`
  (`ifc2lbd-cli/src/pipeline_plugins.rs:211-455`). **This is what the CLI runs, and
  therefore what cn3-pt1 ingests.**
- **Monolithic path** — `stream_step_and_model` → `stream_lbd` → `emit_lbd`
  (`lbd-converter/src/lib.rs:173-300`). Used by the **WASM runner**
  (`ifc2lbd-wasm/src/runner.rs:826, 915, 997, 999`).

The named graphs are built from the model's `base_uri`. They have nothing to do
with the `https://linkedbuildingdata.org/LBD#` vocabulary namespace — the two are
unrelated concepts that happen to share the letters "LBD".

Every fix below is applied at a point shared by both paths (`lbd-ontology`
constants, `spatial_class`, `lbd_product_class_iri`) **except** §5, which lives in
`emit_bot` and so only affects the modular path — correctly, since the monolithic
path does not emit spatial-hierarchy predicates at all.

### B.2 The live `LBD#` surface is one term

| term | emitted from | reachable in |
| --- | --- | --- |
| `lbd:Project` | `spatial_class()` → `emit_bot` (`modules/bot.rs:34`) | modular path |
| `lbd:hasBoundingBox` | `emit_bounding_box_geometries` (`lib.rs:2088`, called at `lib.rs:696`) | monolithic path only, and only when `geometry_bounding_boxes`/`geometry_wkts` is populated |

`lbd:PropertySet`, `lbd:hasPropertySets`, `lbd:ElementQuantitySet`,
`lbd:hasQuantitySet`, `lbd:x-min`…`lbd:z-max` (`lbd-ontology/src/lib.rs:113-159`)
are **called from nowhere**. Props emits `props:` + `opm:` only
(`modules/props_opm.rs`), and its own doc comment states that property-set
container nodes belong to the bSDD graph.

### B.3 New finding — `beo:Furniture` is emitted too, and BEO does not declare it

`ifc-schema/src/lib.rs:231` maps `IFCFURNITURE` and `IFCSYSTEMFURNITUREELEMENT` to
product type `Furniture`, but `lbd_product_class_iri` (`lib.rs:2432-2437`) only
routes `IFCFURNISHINGELEMENT` into the `furn:` namespace:

```rust
match entity_name {
    "IFCFURNISHINGELEMENT" => furn_class(product_type),
    _ => beo_class(product_type),          // IFCFURNITURE lands here
}
```

So an IFC4 model using `IFCFURNITURE` emits **`beo:Furniture`** — an undescribed
term in a *live* namespace, which is worse than `furn:Furniture` in a dead one. The
handoff's audit models evidently used `IFCFURNISHINGELEMENT`, so this never
surfaced. The §1 allowlist must therefore guard the **base class**, not only the
predefined-type suffix (see W2).

### B.4 The predefined-type suffix is unguarded in more ways than one

`ifc-model/src/lib.rs:636-655` ends in a catch-all:

```rust
_ => optional_enum(entity.args.get(8)),
```

Any element type whose arg 8 happens to be an enum yields a `predefined_type`,
which `emit_beo` then appends verbatim. `NOTDEFINED` is the visible symptom;
`USERDEFINED` and arbitrary mis-read enums are the same bug. An allowlist closes
all of them at once; a `NOTDEFINED` denylist closes one.

---

## C. Work items

### W1 — Vendor BEO's declared class list  *(prerequisite for W2)*

**Why a generated list rather than the raw TTL:** `include_str!`-ing BEO's full
Turtle would add a few hundred KB to the WASM binary and cost a parse at first use.
We need only the set of declared class local names.

1. Fetch BEO source of truth. Canonical: `https://pi.pauwel.be/voc/buildingelement`
   (content negotiation) — mirror at `github.com/pipauwel/BEO`. **Pin the version
   and record it**; BEO is versioned and the allowlist must be reproducible.
2. Store the raw TTL at `ontologies/beo.ttl` for provenance, matching the existing
   `ontologies/bsddm.ttl`.
3. Add `scripts/build_beo_index.py` — same role and shape as the existing
   `scripts/build_bsdd_index.py`. It extracts every subject declared
   `owl:Class` / `rdfs:Class` in the `https://pi.pauwel.be/voc/buildingelement#`
   namespace and writes sorted local names, one per line, to
   `crates/lbd-converter/resources/beo_classes.txt`.
4. `include_str!` that file in `lbd-converter` and build a
   `OnceLock<HashSet<&'static str>>` accessor:

   ```rust
   fn beo_declared_classes() -> &'static HashSet<&'static str> { … }
   pub(crate) fn beo_declares(local_name: &str) -> bool { … }
   ```

   `OnceLock` + `&'static str` borrowed from the included string means no
   allocation per name and no cost on models that never hit it.
5. **Check BEO's licence** and add the attribution to `NOTICE` if required. Do not
   skip this — the repo already tracks third-party licences deliberately
   (`LICENSE`, `LICENSE-MPL`, `NOTICE`).

**Sanity check on the generated list:** it must contain `Railing`, `Stair`, `Roof`,
`Slab`, `BuildingElement`, `Railing-BALUSTRADE`, `Railing-GUARDRAIL`,
`Railing-HANDRAIL`; and must **not** contain `Railing-NOTDEFINED` or `Furniture`.
Assert exactly this in a unit test so a bad regeneration fails loudly.

### W2 — Guard every BEO/product-class type against the allowlist  *(§1 + §3 + B.3)*

`crates/lbd-converter/src/lib.rs:2432` — `lbd_product_class_iri` becomes fallible
and drops the `furn:` branch:

```rust
pub(crate) fn lbd_product_class_iri(product_type: &str) -> Option<String> {
    beo_declares(product_type).then(|| beo_class(product_type))
}
```

`crates/lbd-converter/src/modules/beo.rs:26-44` — guard base class and suffix
independently:

```rust
for element in sorted_values(&model.elements) {
    let Some(product_type) = product_type_name(element.entity_name.as_str()) else { continue };
    let Some(product_class) = lbd_product_class_iri(product_type) else { continue };
    let subject = element_resource_iri(base, element);
    if let Some(pt) = element.predefined_type.as_ref() {
        let sub_class = format!("{product_class}-{pt}");
        if beo_declares_iri(&sub_class) {
            emit(Triple { subject: subject.clone(), predicate: rdf_type(),
                          object: Object::Iri(sub_class) })?;
        }
    }
    emit(Triple { subject, predicate: rdf_type(), object: Object::Iri(product_class) })?;
}
```

Net effect:
- `beo:Railing-NOTDEFINED` → `beo:Railing` (§1, all five reported IRIs).
- `-USERDEFINED` and stray-enum suffixes → silently dropped, base class kept.
- `furn:Furniture` → nothing; element keeps `bot:Element` (§3).
- `beo:Furniture` (from `IFCFURNITURE`) → nothing (B.3).

**This is the one item that silently removes triples.** Log a `debug!` count of
suppressed types per run so a future BEO version bump that drops a class is
diagnosable rather than invisible.

### W3 — Remove the `furn:` namespace  *(§3)*

`crates/lbd-ontology/src/lib.rs`: delete `FURN` (line 7), `furn_class()` (193-195),
and the `("furn", FURN)` entry from `PREFIXES` (line 33).

### W4 — `smls:unit` → `qudt:unit`  *(§2)*

`crates/lbd-ontology/src/lib.rs`:
- Delete `SMLS` (line 19), `smls_unit()` (253-255), and the `("smls", SMLS)`
  prefix entry.
- Add `pub const QUDT: &str = "http://qudt.org/schema/qudt/";` and
  `pub fn qudt_unit() -> String { format!("{QUDT}unit") }`, plus a
  `("qudt", QUDT)` prefix entry.

Note the existing `UNIT = "http://qudt.org/vocab/unit/"` (prefix `unit`) is a
*different* QUDT namespace — it holds unit **individuals** (`unit:MilliM`), which
are the objects of this predicate. Both are needed; do not merge them.

Call sites to update: `lib.rs:1741`, `modules/bsdd.rs:2285`, `modules/bsdd.rs:2524`,
and the test assertions at `lib.rs:2929`.

**`PREFIXES` is a fixed-size array** — `[(&str, &str); 22]` at
`lbd-ontology/src/lib.rs:26`. W3 removes 2 (`furn`, `smls`), W4 adds 1 (`qudt`),
W5 adds 1 (`dicp`) → **22**. It happens to balance; still, update the literal
deliberately rather than by luck. Consumers: `lbd-serializer/src/lib.rs:148, 409,
447, 468`.

### W5 — `LBD#Project` → `dicp:ConstructionProject`  *(§4)*

> **Blocked on §F.0.** If the project node is dropped instead of retyped, this item
> and W6 both disappear. Do not implement either until that is answered.

`crates/lbd-ontology/src/lib.rs`:
- Add `pub const DICP: &str = "https://w3id.org/digitalconstruction/0.5/Processes#";`
  and `pub fn dicp_construction_project() -> String { format!("{DICP}ConstructionProject") }`
  plus the `("dicp", DICP)` prefix entry.
- Leave `LBD`, `lbd_project()` and the unused `LBD#` fns in place (per decision).

`crates/lbd-converter/src/lib.rs:2275` — `SpatialType::Project => dicp_construction_project()`.

`lbd_project()` becomes unused after this and will trip `dead_code`. Either delete
it or mark it; deleting is cleaner and does not touch the `LBD` const that
`lbd:hasBoundingBox` still needs.

### W6 — `bot:hasSite` → `bot:containsZone`  *(§5)*

`crates/lbd-converter/src/modules/bot.rs:64` —
`(SpatialType::Project, SpatialType::Site) => Some(bot_contains_zone())`.

`bot_has_site()` (`lbd-ontology/src/lib.rs:293-295`) becomes unused → delete.
Update the `bot:hasSite` fixture IRIs in the `lbd-serializer` tests
(`lbd-serializer/src/lib.rs:605, 612, 647, 662, 670, 675`) — they are arbitrary
test data, but leaving a retired predicate in fixtures invites confusion.

See §F.1 before implementing.

### W7 — Housekeeping

- `CHANGELOG.md` entry covering W2–W6, explicitly marked **breaking** for W2, W4,
  W5, W6.
- Bump `workspace.package.version` (currently `0.3.5`).
- Move `handoff-ifc2lbd-neo-vocabulary-fixes.md` and this plan into `docs/` — the
  handoff's own header asks for it not to sit at repo root long-term.
- `cargo check --workspace` per `AGENTS.md` §11, plus `cargo clippy` (W3/W5/W6 each
  orphan a function).

---

## D. Verification

### Unit / integration

| Item | Test |
| --- | --- |
| W1 | Allowlist contains `Railing`, `Railing-HANDRAIL`; excludes `Railing-NOTDEFINED`, `Furniture`. |
| W2 | `IFCRAILING` + `.NOTDEFINED.` → exactly `beo:Railing` + `bot:Element`, no `-NOTDEFINED`. |
| W2 | `IFCFURNISHINGELEMENT` and `IFCFURNITURE` → `bot:Element` only, no product-class type. |
| W4 | Property state with a unit → `qudt:unit`; no triple anywhere has an `smls:` predicate. |
| W5 | Project node typed `dicp:ConstructionProject`; `LBD#Project` absent. |
| W6 | Project→Site link is `bot:containsZone`; `bot:hasSite` absent. |

### End-to-end

Convert a real model, then:

```bash
zcat out.nq.gz | grep -c 'NOTDEFINED'           # expect 0        (§1)
zcat out.nq.gz | grep -c 'smls-owl'             # expect 0        (§2)
zcat out.nq.gz | grep -c 'voc/furniture'        # expect 0        (§3)
zcat out.nq.gz | grep -c 'linkedbuildingdata'   # expect 0        (§4)
zcat out.nq.gz | grep -c 'bot#hasSite'          # expect 0        (§5)
```

All five are path-independent — run the same over the `.ttl` output.

Non-ASCII literals should still be present as **raw UTF-8** — that is the correct,
spec-conformant output and is deliberately unchanged:

```bash
zcat out.nq.gz | grep -Pc '[^\x00-\x7F]'        # expect > 0 on a German model
```

### From cn3-pt1, after re-ingest

```bash
curl -s localhost:8000/ontology/coverage | jq '.namespaces[] | select(.undescribed | length > 0)'
```

Target: empty for `ifc-*` namespaces.

Separately, and independent of this plan: a German property value reads back as
`Türöffnung`, not `TÃ¼rÃ¶ffnung` — that verifies the loader JVM charset fix
(§A.1), not anything in this repo.

---

## E. Sequencing

W1+W2 are independent of cn3-pt1 and can ship immediately — they only remove
triples that nothing resolves anyway.

W4, W5, W6 are breaking for cn3-pt1 queries and want one coordinated release +
re-ingest. The handoff states cn3-pt1 will be ready first.

Suggested: **one release containing everything**, announced as breaking, followed
by a coordinated re-ingest.

The §6 charset fix is on the cn3-pt1 side and is not coupled to this release — it
can land before, during or after, and needs no re-ingest coordination with the
converter beyond re-loading affected models.

---

## F. Open items

### F.-1 Pre-existing problems found while implementing

Neither was caused by this work; both were fixed or are noted because they affect
anyone verifying it.

1. **`lbd-converter`'s test target did not compile at `314adcb`.** `rdf_member()`
   was used in a test but never imported. Fixed — otherwise `cargo test -p
   lbd-converter` cannot run at all.
2. **`test_convert_model_emits_bot_hierarchy` is dormant and wrong.** It asserts
   `bot:hasBuilding` / the project→site edge on output from `convert_step_and_model`,
   i.e. the monolithic `emit_lbd` path — which does not emit spatial-hierarchy
   predicates at all (only `emit_bot` does, per §B.1). It passes only because
   `Duplex.ifc` is absent so it early-returns. Its `bot_has_site()` assertion was
   updated to `bot_contains_zone()` so it compiles, but it would fail if the
   fixture were present, for reasons predating this change. **Not fixed** — doing
   so means rewriting it against `stream_bot`, which is outside this plan.
3. `ifc-lite-geometry` / `ifc-lite-core` test targets fail to compile (E0433) in
   `vendor/`. Untouched, unrelated.

### F.0 Should the project node exist at all?  *(resolved as Option B — see Status)*

The handoff assumes the node stays and asks only for a better type. That assumption
deserves a check, because the node may not earn its place.

**What is and isn't invented.** `IFCPROJECT` is a real STEP entity with a GUID,
name and description — the *node* is faithful. What was invented is the *class*
`https://linkedbuildingdata.org/LBD#Project`, minted by the original Java IFC2LBD
converter to paper over the fact that **BOT has no project concept by design**;
BOT's hierarchy starts at `bot:Site`.

**The node is a duplicate.** `stream_ifcowl` iterates every STEP entity with no
filtering (`modules/ifcowl.rs:48-53`), so `IfcProject` — GlobalId, Name,
Description — is already fully represented in the `…/ifcowl` graph. Dropping the
LBD-side node loses no data.

What the node carries today, all of it recoverable from ifcOWL:

| triple | from | graph |
| --- | --- | --- |
| `rdf:type lbd:Project` | `spatial_class` → `emit_bot` (`modules/bot.rs:34`) | `…/bot` |
| `bot:hasSite <site>` | `modules/bot.rs:64` — the §5 bug, and its only use | `…/bot` |
| `props:globalIdIfcRoot` / `nameIfcRoot` / `descriptionIfcRoot` | `emit_standard_attribute_triples` iterates all spatial nodes (`lib.rs:1760`) | `…/props` |
| bSDD mapping as `IfcProject` | `modules/bsdd.rs:938` | `…/bsdd` |
| `omg:hasGeometry` + `omg:Geometry` | `modules/omg_fog.rs:32-45`, which types every spatial node indiscriminately | `…/omg` |

**Option A — drop the project node from `model.spatial_nodes`.** Closes three
items at once: §4 becomes moot (nothing to retype, no `dicp` prefix needed), §5
becomes moot (the project→site edge is the *only* `hasSite` use, so it disappears
rather than being renamed), and F.1 below becomes moot (no BOT edge with a
non-Zone subject). Also removes the odd `omg:hasGeometry` on a project.

**Option B — keep it, retype `dicp:ConstructionProject`.** What W5/W6 as written
assume. Preserves a single traversable root and whatever cn3-pt1 queries hang off
it.

**The deciding question, and it can only be answered in cn3-pt1:** do any queries
traverse from or match on the project root? If not, take Option A — it is strictly
less code and strictly fewer undescribed terms. If yes, Option B.

Ask cn3-pt1 before implementing W5 or W6.

### F.1 `bot:containsZone` has `rdfs:domain bot:Zone`  *(only applies under F.0 Option B)*

If the project node stays, W5 types it `dicp:ConstructionProject` and W6 then makes
that same node the subject of `bot:containsZone`, whose BOT domain is `bot:Zone`.
Under a reasoner — and this repo ships `crates/owl-reasoner` — that infers the
construction project *is* a `bot:Zone`, which is wrong.

Under F.0 Option A this cannot arise, since there is no project→site edge at all.

The handoff anticipates this: *"or belongs outside BOT entirely if the subject is
the project rather than a zone."*

Options:
1. Ship `bot:containsZone` anyway. It is what the handoff asks for, it is a real
   BOT property, and the bad inference only materialises if someone reasons over
   the BOT graph with BOT's own axioms loaded.
2. Use a DiCP-side property for project→site and leave BOT out of that one edge.
   Requires checking what DiCP 0.5 offers and confirming cn3-pt1 can query it.

**Recommendation: (1) now, revisit if reasoning is actually enabled over that
graph.** Flag it to cn3-pt1 either way so the choice is theirs to object to.

### F.2 BEO version pinning

W1's allowlist is only as good as the BEO snapshot it was generated from. Record
the version/commit in `ontologies/beo.ttl`'s header and in the generator script. A
BEO release that *adds* predefined-type variants would silently keep suppressing
them until the list is regenerated — hence the `debug!` counter in W2.

### F.3 Dead `LBD#` constants left in place

Per decision, `lbd:PropertySet`, `lbd:hasPropertySets`, `lbd:ElementQuantitySet`,
`lbd:hasQuantitySet` and `lbd:x-min`…`z-max` stay. They emit nothing today but are
public API of `lbd-ontology`, so a future contributor reaching for
`lbd_property_set()` would reintroduce a dead-namespace term without noticing.
Worth a `#[deprecated]` note or a comment pointing at this plan, even if they are
not removed.

### F.4 `lbd:hasBoundingBox` remains undescribed

It only reaches output via the WASM/monolithic path with bboxes enabled, so
cn3-pt1's audit does not see it. If bbox output is ever wired into the modular
pipeline, it will start appearing as an undescribed term and needs the same
treatment as W5.
