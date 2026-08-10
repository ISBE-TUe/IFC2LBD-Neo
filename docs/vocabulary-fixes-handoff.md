# IFC2LBD-Neo: fixes requested by cn3-pt1

**For the `ifc2lbd-neo` repo.** Move this file there; it does not belong in
`cn3-pt1` long-term.

Two groups: §1–§5 are vocabulary terms that resolve to nothing (found
2026-08-05); §6 is an N-Quads serializer encoding bug that cn3-pt1 was working
around at a cost of ~33 s per model until 2026-08-10.

## Vocabulary terms that resolve to nothing (§1–§5)

Found on 2026-08-05 by `GET /ontology/coverage` in cn3-pt1, which compares every
type and predicate in a converted namespace against the vocabulary seeded
alongside it. Measured on two real models.

### Why these matter

A type that no vocabulary describes behaves as a string. `rdfs:subClassOf*`
finds no ancestors, the UI renders a raw IRI, SHACL cannot target it, and an
agent asked what the thing *is* has nothing to read. Nothing errors — the
triples load and the counts look right — so these survived months of use before
an audit found them.

The platform side is now clean: after fixing our own writers, **every remaining
undescribed term in a converted namespace comes from this converter.**

---

## 1. `beo:{Element}-NOTDEFINED` — invented classes

**339 instances in one model.** When IFC `PredefinedType` is `NOTDEFINED`, the
converter appends `-NOTDEFINED` to the BEO class name. BEO does not define those
variants.

| emitted | instances | should be | in BEO? |
| --- | ---: | --- | --- |
| `beo:Railing-NOTDEFINED` | 287 | `beo:Railing` | ✅ declared |
| `beo:Stair-NOTDEFINED` | 22 | `beo:Stair` | ✅ declared |
| `beo:Roof-NOTDEFINED` | 21 | `beo:Roof` | ✅ declared |
| `beo:Slab-NOTDEFINED` | 9 | `beo:Slab` | ✅ declared |
| `beo:BuildingElement-NOTDEFINED` | 1 | `beo:BuildingElement` | ✅ declared |

BEO *does* ship the other predefined-type variants — `beo:Railing-BALUSTRADE`,
`beo:Railing-GUARDRAIL`, `beo:Railing-HANDRAIL` — so the pattern is right and
only the `NOTDEFINED` case is wrong. NOTDEFINED means "no subtype stated", which
is the base class, not a subtype called NOTDEFINED.

**Fix:** when `PredefinedType` is `NOTDEFINED` (or absent), emit the base class.
Suggest guarding it generally: only append a predefined-type suffix when the
resulting IRI exists in BEO, and fall back to the base class otherwise. That
stops the whole family of this bug rather than these five cases.

**Verify:** all five IRIs above should disappear from
`GET /ontology/coverage` after a re-ingest.

---

## 2. `smls:unit` — 20,788 uses of a dead vocabulary

**The largest of these by a wide margin.** `https://w3id.org/def/smls-owl#unit`
carries the unit on OPM property states. The namespace **returns 404** — checked
2026-08-05, `https://w3id.org/def/smls-owl#` is gone. There is no vocabulary to
vendor and no prospect of one.

Seven production queries in cn3-pt1 depend on the predicate, so this is not a
quiet corner.

**Recommended fix: emit `qudt:unit` instead** —
`http://qudt.org/schema/qudt/unit`. QUDT is maintained, resolvable, already
declared in cn3-pt1's namespace registry, and already used by
`get-building-overview.rq` for storey elevation units. It is the standard answer
to exactly this question.

**This is a breaking change** and needs sequencing with cn3-pt1:

1. converter emits `qudt:unit` alongside `smls:unit` (both, one release);
2. cn3-pt1 updates its seven queries and vendors QUDT;
3. converter drops `smls:unit`;
4. existing models re-ingested.

If dual-emitting is unattractive, a hard switch plus a coordinated re-ingest is
fine — the data is derived, nothing is authored in it. Say which you prefer and
the cn3-pt1 side will be ready first.

---

## 3. `furn:Furniture` — dead host, and no replacement in BEO

**372 instances.** `http://pi.pauwel.be/voc/furniture#` returns *"No website is
present on this hostname"*. Note it is `http://`, while BEO on the same domain
is `https://pi.pauwel.be/voc/buildingelement#` and does resolve — so this is not
a scheme typo, the furniture vocabulary is simply gone.

**There is no obvious replacement.** BEO has no furniture class at all — checked,
zero matches for `Furni` in the whole ontology. So the converter cannot just
point somewhere better.

Options, roughly in order of preference:

1. **`bot:Element` only.** All 372 are already typed `bot:Element`, so dropping
   `furn:Furniture` loses the "it is furniture" distinction but leaves nothing
   dangling. Cheapest, and honest.
2. **Keep `furn:Furniture` and let cn3-pt1 describe it** in an alignment file it
   authors. Preserves the distinction; the IRI still will not dereference.
3. **Mint an IFC2LBD-Neo term** for furniture in a namespace this project
   controls and can actually serve.

Lowest priority of the five — the redundant `bot:Element` typing means nothing
is broken today.

---

## 4. `LBD#Project` — an entire namespace with nothing behind it

**1 instance per model.** `https://linkedbuildingdata.org/LBD#Project` is the
root node of the converted graph, and `linkedbuildingdata.org/LBD` is not a
vocabulary cn3-pt1 has, knows about, or can resolve. It is not in the namespace
registry at all.

Given it is the *root* of every converted model, it deserves a real type. BOT
has no project concept, but cn3-pt1 models projects as
`dicp:ConstructionProject` (Digital Construction Ontology 0.5,
`https://w3id.org/digitalconstruction/0.5/Processes#ConstructionProject`), which
is vendored and resolvable.

**Fix:** either emit `dicp:ConstructionProject`, or mint the root in a namespace
this project owns and publishes. Anything is better than a namespace with no
document behind it.

---

## 5. `bot:hasSite` — not a BOT property

**1 use per model.** BOT defines these object properties, and `hasSite` is not
among them:

```
bot:hasBuilding   bot:hasElement    bot:hasSimple
bot:hasSpace      bot:hasStorey     bot:hasSubElement    bot:hasZeroPoint
```

In BOT, `bot:Site` is a `bot:Zone`, and zone containment is
`bot:containsZone`. The link from the project root to the site is probably
`bot:containsZone`, or belongs outside BOT entirely if the subject is the
project rather than a zone.

**Fix:** use `bot:containsZone`, or model the project→site link with a property
that exists.

---

---

## 6. `neo-nquads-serializer` — escape non-ASCII literals as `\uXXXX`

**Every model with a non-ASCII character in it, which in practice means every
German model.** This one is not about vocabulary; it is about bytes.

### Symptom

`ifc2lbd-neo` writes non-ASCII characters in N-Quads string literals as raw
UTF-8 bytes. Blazegraph's bulk `DataLoader` (Java) reads `.nq` files as Latin-1,
so each UTF-8 byte pair becomes two characters and the value lands in the store
double-encoded: `Türöffnung` → `TÃ¼rÃ¶ffnung`. Nothing errors — the load
succeeds, the triple counts are right — and the mangling only shows up when a
human reads a property panel.

### Fix

In `neo-nquads-serializer`, when writing a **string literal**, replace any
codepoint above `U+007F` with its N-Quads escape:

- BMP (`<= U+FFFF`) → `\uXXXX` (4 hex digits, e.g. `ä` → `ä`)
- above the BMP → `\UXXXXXXXX` (8 hex digits)

Leave everything outside string literals alone — IRIs in `<>`, language tags,
and datatype IRIs. `UCHAR` escapes in literals are plain N-Quads
([RDF 1.1 N-Triples §7](https://www.w3.org/TR/n-triples/#grammar-production-UCHAR),
which N-Quads inherits), so every conformant parser accepts them and Blazegraph
decodes them correctly regardless of what charset it thinks the file is in.

Emitting escapes unconditionally is fine — an ASCII-only file is unchanged
either way, so there is no need to detect whether escaping is "needed".

### Why this belongs in the serializer

It is nearly free where you are, and expensive everywhere else. The serializer
already visits every literal and is already streaming into the gzip writer, so
this is a per-character branch on a string it is holding anyway.

cn3-pt1 was doing it after the fact, which meant a second full pass over the
output: gunzip → re-parse each line to track quote state → escape → re-gzip →
atomic rename. Measured on a real model (2026-08-10):

| phase | ms |
| --- | ---: |
| `ifc_convert` (the whole conversion) | 10,527 |
| N-Quads encoding fix (the workaround) | 33,152 |
| Blazegraph bulk load | ~10,200 |

**The workaround cost 3× the conversion itself** and dominated the ingest. It
also had to stream rather than buffer, because the decompressed N-Quads for a
large model exceeds V8's ~512 MiB max string length.

### Status in cn3-pt1

**The workaround was removed on 2026-08-10** (`worker-ifc-ts`:
`src/lib/nquads-encoding.ts` and its test, deleted; the
`nquads_encoding_fix_ms` timing is gone from the convert response `meta`). Until
this lands in the serializer, non-ASCII literals ingest mangled. That is a
deliberate, accepted trade — the fix belongs here, and carrying a 33-second
workaround while waiting for it was not worth it.

### Verify

In the converter, on a model with umlauts:

```bash
zcat out.nq.gz | grep -Pc '[^\x00-\x7F]'   # expect 0
```

From cn3-pt1, after a re-ingest, a German property value reads back intact
(`Türöffnung`, not `TÃ¼rÃ¶ffnung`), and `ifc_convert`'s `meta.timings` shows
`blazegraph_load_ms` at roughly the load time alone.

---

## Not a bug — do not "fix" this one

`schema:value` is written in the **http** namespace (`http://schema.org/value`),
while schema.org's canonical form is `https://`. That is correct and
intentional: both forms coexist in the store, cn3-pt1's `NS.SCHEMA` is the http
form to match, and a blanket http→https normalisation broke the properties panel
once already (cn3-pt1 commit `8011a33`). **Leave it.**

---

## Suggested order

| # | Fix | Effort | Impact |
| --- | --- | --- | --- |
| 6 | escape non-ASCII literals as `\uXXXX` | trivial | ~33 s per ingest, and umlauts are mangled today |
| 1 | `-NOTDEFINED` → base class | trivial | 339 instances, 5 IRIs |
| 5 | `bot:hasSite` → `bot:containsZone` | trivial | correctness of the site link |
| 4 | `LBD#Project` → `dicp:ConstructionProject` | small | root node of every model |
| 2 | `smls:unit` → `qudt:unit` | needs coordination | 20,788 uses |
| 3 | `furn:Furniture` | decision needed | 372 instances, nothing broken today |

## Verifying a vocabulary fix (§1–§5)

From cn3-pt1, after re-ingesting a model:

```bash
curl -s localhost:8000/ontology/coverage | jq '.namespaces[] | select(.undescribed | length > 0)'
```

Every term listed for an `ifc-*` namespace is one this converter emitted and no
vocabulary describes. The target is an empty list. buildingSMART dictionary IRIs
(`identifier.buildingsmart.org/…/prop/…` and `…/class/…`) and LBD `props:`
predicates are excluded by design — they are minted per property and no
ontology could enumerate them.
