# Plan: OWL Reasoning Support for the Ontology Mapper

> **Status:** Implemented (Phase 1). See `crates/owl-reasoner/` for the
> reasoning engine and `crates/ontology-mapper-producer/src/lib.rs` for
> integration via `apply_ontology_mapping()`.

## Problem

The ontology mapper currently does simple 1:1 IRI remapping — it swaps
predicates and classes by looking up IRIs in a HashMap built from
`owl:equivalentProperty`, `owl:equivalentClass`, `rdfs:subPropertyOf`, and
`rdfs:subClassOf` statements where both subject and object are **named
nodes**.

When a user writes:

```turtle
saref4bldg:OwnedBuilding owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        saref4bldg:Building
        [ a owl:Restriction ;
          owl:onProperty ifc:ownerHistory_IfcRoot ;
          owl:someValuesFrom owl:Thing ]
    )
] .
```

the right side is a **blank node** containing a complex OWL class
expression (intersection + existential restriction). The current parser
silently skips it — only named-to-named `equivalentClass` pairs are mapped.

The desired behaviour: for every subject that is a `saref4bldg:Building`
**and** has an `ifc:ownerHistory_IfcRoot` property, add
`<subject> rdf:type saref4bldg:OwnedBuilding` to the output triples.

---

## Scope

### OWL constructs to support

| Construct | RDF Syntax | Semantics |
|-----------|-----------|----------|
| `owl:equivalentClass` (named ↔ expression) | `A owl:equivalentClass [expr]` | A ≡ expr; if subject satisfies expr, infer `rdf:type A` and vice versa |
| `owl:intersectionOf` | `_:b owl:intersectionOf (C1 C2 …)` | Subject must satisfy ALL Ci |
| `owl:unionOf` | `_:b owl:unionOf (C1 C2 …)` | Subject must satisfy ANY Ci |
| `owl:Restriction` + `owl:onProperty` + `owl:someValuesFrom` | `_:b a owl:Restriction; owl:onProperty P; owl:someValuesFrom C` | Subject has ≥1 P-value whose type satisfies C (`owl:Thing` = any IRI value, **not** literals) |
| `owl:Restriction` + `owl:onProperty` + `owl:allValuesFrom` | `_:b a owl:Restriction; owl:onProperty P; owl:allValuesFrom C` | Subject's ALL P-values satisfy C |
| `owl:Restriction` + `owl:onProperty` + `owl:hasValue` | `_:b a owl:Restriction; owl:onProperty P; owl:hasValue V` | Subject has P-value exactly V |
| `owl:Restriction` + `owl:onProperty` + `owl:minCardinality` | `_:b a owl:Restriction; owl:onProperty P; owl:minCardinality N` | Subject has ≥ N distinct P-values (closed-world, deduplicated) |
| `owl:Restriction` + `owl:onProperty` + `owl:maxCardinality` | `_:b a owl:Restriction; owl:onProperty P; owl:maxCardinality N` | Subject has ≤ N distinct P-values (closed-world, deduplicated) |
| `owl:Restriction` + `owl:onProperty` + `owl:cardinality` | `_:b a owl:Restriction; owl:onProperty P; owl:cardinality N` | Subject has exactly N distinct P-values (closed-world, deduplicated) |
| `owl:complementOf` | `A owl:complementOf B` | Subject is NOT of type B |

**Note on `rdfs:subClassOf`:** Only named↔named `subClassOf` is supported
for simple mapping. `A rdfs:subClassOf [expr]` means A ⊑ expr (every A
satisfies expr), **not** the reverse. This is semantically different from
`equivalentClass` (bidirectional) and would require a separate rule
direction. To avoid scope creep, complex `subClassOf` expressions are
deferred to Phase 2. The existing simple `subClassOf` handling (forward +
unambiguous reverse) remains unchanged.

### Explicitly out of scope (Phase 1)

- Complex `rdfs:subClassOf` expressions (blank node right side)
- Transitive `rdfs:subClassOf` closure (reasoning over multi-hop hierarchies)
- `owl:propertyChainAxiom`
- SWRL rules
- SHACL shapes
- Datatype restrictions (xsd:string, xsd:integer constraints)
- Blank-node subject class expressions (only named subjects on the left
  side of `equivalentClass`)

---

## Architecture

### New crate: `owl-reasoner`

Create a new workspace crate `crates/owl-reasoner` with no dependency on
`lbd-pipeline` (pure logic, testable in isolation).

**Layering principle (reviewer S1):** `owl-reasoner` returns only
`Vec<Rule>` — it does **not** contain simple IRI mapping logic. The
existing `build_mapping_tables` / `parse_rdf_mappings` stays in
`ontology-mapper-producer`. This keeps the reasoner pure and avoids
duplicating the align:entity1/entity2 pairing and the unambiguous-reverse
heuristic.

```
crates/owl-reasoner/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Public API: parse_rules, infer_types
│   ├── expression.rs   # ClassExpression enum + Display
│   ├── parser.rs       # RDF → ClassExpression tree
│   ├── index.rs        # TripleIndex (subject → predicates → objects)
│   └── evaluator.rs    # Evaluate ClassExpression against TripleIndex
└── tests/
    └── integration.rs  # End-to-end tests
```

### Dependencies (reviewer S3: use workspace references)

```toml
[dependencies]
lbd-ontology = { workspace = true }
rio_api = { workspace = true }
rio_turtle = { workspace = true }
oxiri = { workspace = true }
```

No `crossbeam`, no `rayon`, no `lbd-pipeline` — this crate is pure computation.

---

## Data model

### `ClassExpression` enum (expression.rs)

```rust
/// An OWL class expression — a tree of conditions a subject must satisfy.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassExpression {
    /// A named class IRI (e.g. `saref4bldg:Building`)
    Named(String),

    /// owl:intersectionOf — subject must satisfy ALL expressions
    Intersection(Vec<ClassExpression>),

    /// owl:unionOf — subject must satisfy at least ONE expression
    Union(Vec<ClassExpression>),

    /// owl:complementOf — subject must NOT satisfy the expression
    Complement(Box<ClassExpression>),

    /// owl:Restriction
    Restriction(Restriction),
}

/// An OWL restriction on a property.
#[derive(Clone, Debug, PartialEq)]
pub struct Restriction {
    /// The property IRI (owl:onProperty)
    pub property: String,
    /// The kind of restriction
    pub kind: RestrictionKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RestrictionKind {
    /// owl:someValuesFrom — subject has ≥1 value whose type satisfies `class`
    /// `owl:Thing` is represented as `ClassExpression::Named(OWL_THING)`
    SomeValuesFrom(ClassExpression),

    /// owl:allValuesFrom — subject's ALL values satisfy `class`
    AllValuesFrom(ClassExpression),

    /// owl:hasValue — subject has a specific value
    HasValue(lbd_ontology::Object),

    /// owl:cardinality — exactly N distinct values (closed-world)
    ExactCardinality(usize),

    /// owl:minCardinality — at least N distinct values (closed-world)
    MinCardinality(usize),

    /// owl:maxCardinality — at most N distinct values (closed-world)
    MaxCardinality(usize),
}
```

### `Rule` struct (lib.rs)

```rust
/// A reasoning rule: if a subject satisfies `condition`,
/// infer `rdf:type inferred_class`.
#[derive(Clone, Debug)]
pub struct Rule {
    /// The class to infer (left side of equivalentClass with blank node)
    pub inferred_class: String,
    /// The condition expression (right side — must be a complex expression,
    /// not a simple named class; named↔named is handled by the existing
    /// simple mapping)
    pub condition: ClassExpression,
}

/// The output of the reasoner: new rdf:type triples to add.
#[derive(Clone, Debug)]
pub struct InferredTriple {
    pub subject: String,
    pub object: lbd_ontology::Object,  // the inferred class IRI
    // predicate is always rdf:type — stored as a constant, not per-triple (reviewer S4)
}

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
```

---

## Parser (parser.rs)

### RDF List parsing

`owl:intersectionOf` and `owl:unionOf` use RDF lists (collections). In the
raw RDF, a Turtle list `( A B C )` expands to:

```
_:b1 rdf:first A ; rdf:rest _:b2 .
_:b2 rdf:first B ; rdf:rest _:b3 .
_:b3 rdf:first C ; rdf:rest rdf:nil .
```

The parser must follow `rdf:first` / `rdf:rest` chains from a blank node
to build a `Vec<ClassExpression>`.

### Parsing flow

1. **Parse all triples** from the Turtle input into a flat `Vec<rio_triple>`.

2. **Extract rules**: scan for `<named> owl:equivalentClass <blank>` where
   the right side is a **blank node** only. Named↔named `equivalentClass`
   is **excluded** (reviewer W5) — it's handled by the existing simple
   mapping in `ontology-mapper-producer`. If the right side is a named
   node, skip it (no rule generated).

3. **Build expressions**: for each rule's right side, recursively parse:
   - If it's a named node → `ClassExpression::Named(iri)`
   - If it's a blank node:
     - Check for `owl:intersectionOf` → follow RDF list, parse each element
     - Check for `owl:unionOf` → follow RDF list, parse each element
     - Check for `owl:complementOf` → parse the object as ClassExpression
     - Check for `owl:Restriction`:
       - Read `owl:onProperty` → property IRI
       - Read `owl:someValuesFrom` → parse as ClassExpression
       - Read `owl:allValuesFrom` → parse as ClassExpression
       - Read `owl:hasValue` → parse as Object (IRI or literal)
       - Read `owl:cardinality` / `owl:minCardinality` / `owl:maxCardinality`
         → parse as xsd:nonNegativeInteger

4. **Fallback**: if a blank node doesn't match any known OWL construct,
   skip it and return a warning string (not an error). The caller
   continues with whatever rules were successfully parsed.

### Key constants

```rust
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_RESTRICTION: &str = "http://www.w3.org/2002/07/owl#Restriction";
const OWL_ON_PROPERTY: &str = "http://www.w3.org/2002/07/owl#onProperty";
const OWL_SOME_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#someValuesFrom";
const OWL_ALL_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#allValuesFrom";
const OWL_HAS_VALUE: &str = "http://www.w3.org/2002/07/owl#hasValue";
const OWL_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#cardinality";
const OWL_MIN_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#minCardinality";
const OWL_MAX_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#maxCardinality";
const OWL_INTERSECTION_OF: &str = "http://www.w3.org/2002/07/owl#intersectionOf";
const OWL_UNION_OF: &str = "http://www.w3.org/2002/07/owl#unionOf";
const OWL_COMPLEMENT_OF: &str = "http://www.w3.org/2002/07/owl#complementOf";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
```

---

## Triple Index (index.rs)

Build an in-memory index for efficient evaluation. The index stores
references (or clones) of the `Triple` data in a structured form.

**Reviewer W3: add reverse index.** `subjects_of_type(class)` must be
O(1), not O(N). Add a `by_type` reverse map.

```rust
/// Index of all triples by subject, enabling fast restriction evaluation.
pub struct TripleIndex {
    /// subject IRI → (predicate IRI → Vec<Object>)
    by_subject: HashMap<String, HashMap<String, Vec<Object>>>,
    /// subject IRI → Set of rdf:type class IRIs
    types: HashMap<String, HashSet<String>>,
    /// class IRI → Set of subject IRIs (reverse index, reviewer W3)
    by_type: HashMap<String, HashSet<String>>,
}
```

### Construction

```rust
impl TripleIndex {
    pub fn from_triples(triples: &[Triple]) -> Self {
        // Single pass: populate by_subject, types, by_type
    }

    /// Does `subject` have `rdf:type class`?
    pub fn has_type(&self, subject: &str, class: &str) -> bool { ... }

    /// Get all values of `predicate` for `subject`
    pub fn get_objects(&self, subject: &str, predicate: &str) -> &[Object] { ... }

    /// Get all subjects that have `rdf:type class` — O(1) via by_type
    pub fn subjects_of_type(&self, class: &str) -> Option<&HashSet<String>> { ... }

    /// Count distinct values of `predicate` for `subject` (reviewer W2)
    pub fn count_distinct_objects(&self, subject: &str, predicate: &str) -> usize { ... }

    /// Does `subject` have any IRI-valued triple with `predicate`?
    pub fn has_iri_property(&self, subject: &str, predicate: &str) -> bool { ... }
}
```

### Memory (reviewer W10: realistic estimate)

The index clones every subject, predicate, and object `String` into
`by_subject` plus duplicates class IRIs in `types`/`by_type`. Realistic
cost: 3–5× raw triple data. For 2M triples (~200 MB), the index is
~600 MB–1 GB. This is tight for the WASM 4 GB cap (~2 GB usable after
runtime/serializer). The `needs_full_graph: true` path already buffers
everything, so the additional memory is the index itself (~3x the
buffered data).

**Mitigation:** Build the index from `&Triple` references where possible
(storing indices into the original `Vec` instead of cloning strings).
If memory is still too tight, add a cap-and-warn: if the triple count
exceeds a threshold (e.g. 5M), log a warning and skip reasoning (simple
mapping still runs).

---

## Evaluator (evaluator.rs)

The core reasoning function:

```rust
/// Evaluate whether `subject` satisfies `expression` given the triple index.
pub fn evaluate(
    subject: &str,
    expression: &ClassExpression,
    index: &TripleIndex,
) -> bool {
    match expression {
        ClassExpression::Named(iri) => {
            if iri == OWL_THING {
                true // owl:Thing matches everything (but see someValuesFrom below)
            } else {
                index.has_type(subject, iri)
            }
        }

        ClassExpression::Intersection(parts) => {
            parts.iter().all(|p| evaluate(subject, p, index))
        }

        ClassExpression::Union(parts) => {
            parts.iter().any(|p| evaluate(subject, p, index))
        }

        ClassExpression::Complement(inner) => {
            !evaluate(subject, inner, index)
        }

        ClassExpression::Restriction(r) => {
            match &r.kind {
                RestrictionKind::SomeValuesFrom(class_expr) => {
                    let objects = index.get_objects(subject, &r.property);
                    if is_owl_thing(class_expr) {
                        // Reviewer W1: owl:Thing matches IRI values only, NOT literals
                        objects.iter().any(|obj| matches!(obj, Object::Iri(_)))
                    } else {
                        objects.iter().any(|obj| {
                            evaluate_object(obj, class_expr, index)
                        })
                    }
                }

                RestrictionKind::AllValuesFrom(class_expr) => {
                    let objects = index.get_objects(subject, &r.property);
                    if objects.is_empty() {
                        // Vacuously true: no values means all values satisfy
                        true
                    } else {
                        objects.iter().all(|obj| {
                            evaluate_object(obj, class_expr, index)
                        })
                    }
                }

                RestrictionKind::HasValue(value) => {
                    index.get_objects(subject, &r.property)
                        .iter()
                        .any(|obj| obj == value)
                }

                RestrictionKind::ExactCardinality(n) => {
                    index.count_distinct_objects(subject, &r.property) == *n
                }

                RestrictionKind::MinCardinality(n) => {
                    index.count_distinct_objects(subject, &r.property) >= *n
                }

                RestrictionKind::MaxCardinality(n) => {
                    index.count_distinct_objects(subject, &r.property) <= *n
                }
            }
        }
    }
}

/// Evaluate whether an Object (IRI or literal) satisfies a class expression.
fn evaluate_object(
    obj: &Object,
    expression: &ClassExpression,
    index: &TripleIndex,
) -> bool {
    match obj {
        Object::Iri(iri) => evaluate(iri, expression, index),
        Object::Literal(_) | Object::TypedLiteral { .. } => {
            // Literals can only satisfy owl:Thing at the top level.
            // Complex datatype restrictions are out of scope (Phase 1).
            matches!(expression, ClassExpression::Named(iri) if iri == OWL_THING)
        }
    }
}
```

### Candidate filtering (reviewer C3: structurally aware)

The naive "collect all named classes and intersect candidates" is
**unsound** for `unionOf` and `complementOf`. Instead, use a
structurally-aware filter:

```rust
/// Compute the candidate subject set for a rule.
/// For Intersection: intersect candidates from each part.
/// For Union: union candidates from each part.
/// For Complement: ALL subjects (can't pre-filter a negation).
/// For Named: subjects_of_type.
/// For Restriction: depends — someValuesFrom/hasValue filter by property
///   existence; allValuesFrom/cardinality also need property existence.
fn candidate_subjects(
    expression: &ClassExpression,
    index: &TripleIndex,
    all_subjects: &[&str],
) -> Vec<String> {
    match expression {
        ClassExpression::Named(iri) => {
            index.subjects_of_type(iri)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default()
        }
        ClassExpression::Intersection(parts) => {
            let mut sets: Vec<HashSet<String>> = parts.iter()
                .map(|p| candidate_subjects(p, index, all_subjects).into_iter().collect())
                .collect();
            // Intersect all sets
            if let Some(mut result) = sets.pop() {
                for s in sets {
                    result.retain(|x| s.contains(x));
                }
                result.into_iter().collect()
            } else {
                Vec::new()
            }
        }
        ClassExpression::Union(parts) => {
            let mut result = HashSet::new();
            for p in parts {
                for s in candidate_subjects(p, index, all_subjects) {
                    result.insert(s);
                }
            }
            result.into_iter().collect()
        }
        ClassExpression::Complement(_) => {
            // Can't pre-filter negation — must evaluate all subjects
            all_subjects.iter().map(|s| s.to_string()).collect()
        }
        ClassExpression::Restriction(r) => {
            // Filter to subjects that have the restricted property
            all_subjects.iter()
                .filter(|s| index.has_iri_property(s, &r.property))
                .map(|s| s.to_string())
                .collect()
        }
    }
}
```

---

## Public API (lib.rs)

```rust
/// Parse alignment + ontology files for complex OWL reasoning rules.
///
/// Returns only rules for `equivalentClass` with blank-node (complex)
/// right sides. Named↔named mappings are handled by the caller
/// (`ontology-mapper-producer::build_mapping_tables`).
///
/// Unknown blank node constructs are skipped with a warning — they do
/// NOT cause an error (reviewer W8: simple mapping must still work).
pub fn parse_rules(
    alignment_turtle: &str,
    ontology_turtle: &str,
) -> Result<(Vec<Rule>, Vec<String>), String>;
// Returns (rules, warnings)

/// Run the reasoner over a set of triples.
///
/// Builds an index, evaluates each rule's condition against candidate
/// subjects, and returns new `rdf:type` triples for satisfied conditions.
/// Existing `rdf:type` assertions are NOT duplicated (reviewer W6).
///
/// Single pass — no fixpoint iteration (reviewer W4: documented limitation).
/// Rules whose conditions reference classes inferred by other rules
/// will not fire. Users should order alignment axioms so that base
/// `equivalentClass` mappings come before derived ones.
pub fn infer_types(
    rules: &[Rule],
    triples: &[Triple],
) -> Vec<InferredTriple>;
```

---

## Integration into the ontology mapper plugin

### Critical fix (reviewer C2): reasoning must run AFTER simple mapping

The simple IRI remapping produces new `rdf:type` triples (e.g.
`ifc:IfcBuilding → saref4bldg:Building`). The OWL reasoner must see
these remapped types to evaluate conditions like
`intersectionOf(saref4bldg:Building, …)`. Therefore:

1. Apply simple mappings first, producing the remapped triple set.
2. Build the reasoner index from the **union of original + remapped triples**.
3. Run reasoning on that combined index.

### Postprocess plugin (lib.rs)

```rust
fn postprocess(
    &self,
    ctx: &PipelineContext,
    batches: &mut Vec<TaggedBatch>,
) -> Result<(), PostprocessError> {
    let config = ctx.get::<OntologyMappingConfig>()...;
    let options = ctx.get::<ConvertOptions>()...;

    // 1. Build simple mapping tables (existing behaviour, unchanged)
    let tables = engine::build_mapping_tables(
        &config.alignment_turtle, &config.ontology_turtle,
    ).map_err(PostprocessError::Postprocessing)?;

    // 2. Build reasoning rules (new — may fail without affecting step 1)
    let (rules, rule_warnings) = owl_reasoner::parse_rules(
        &config.alignment_turtle, &config.ontology_turtle,
    ).unwrap_or_else(|e| {
        // Reviewer W8: parse failure must not regress simple mapping.
        // Log warning, continue with no rules.
        (Vec::new(), vec![format!("OWL reasoning skipped: {e}")])
    });

    // 3. Apply simple IRI remapping (existing behaviour, changed-only filter — reviewer W7)
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let mut mapped_triples: Vec<Triple> = Vec::new();
    for batch in batches.iter() {
        for triple in &batch.triples {
            let mapped_predicate = tables.property_map
                .get(&triple.predicate).cloned()
                .unwrap_or_else(|| triple.predicate.clone());
            let mapped_object = if triple.predicate == rdf_type {
                match &triple.object {
                    Object::Iri(iri) => tables.class_map
                        .get(iri).map(|c| Object::Iri(c.clone()))
                        .unwrap_or_else(|| triple.object.clone()),
                    _ => triple.object.clone(),
                }
            } else { triple.object.clone() };
            // Changed-only filter (reviewer W7)
            if mapped_predicate != triple.predicate || mapped_object != triple.object {
                mapped_triples.push(Triple {
                    subject: triple.subject.clone(),
                    predicate: mapped_predicate,
                    object: mapped_object,
                });
            }
        }
    }

    // 4. Run OWL reasoning on union of original + mapped triples (reviewer C2)
    let mut all_for_reasoning: Vec<Triple> = Vec::new();
    for batch in batches.iter() {
        all_for_reasoning.extend(batch.triples.iter().cloned());
    }
    all_for_reasoning.extend(mapped_triples.iter().cloned());

    let inferred = owl_reasoner::infer_types(&rules, &all_for_reasoning);

    // 5. Add inferred rdf:type triples (dedup — reviewer W6)
    let existing_types: HashSet<(String, String)> = all_for_reasoning.iter()
        .filter(|t| t.predicate == rdf_type)
        .filter_map(|t| match &t.object {
            Object::Iri(iri) => Some((t.subject.clone(), iri.clone())),
            _ => None,
        })
        .collect();

    for inf in inferred {
        if let Object::Iri(class_iri) = &inf.object {
            if !existing_types.contains(&(inf.subject.clone(), class_iri.clone())) {
                mapped_triples.push(Triple {
                    subject: inf.subject,
                    predicate: rdf_type.to_string(),
                    object: inf.object,
                });
            }
        }
    }

    // 6. Push as new batch with ontology graph IRI
    if !mapped_triples.is_empty() {
        batches.push(TaggedBatch {
            kind: BatchKind::new(format!(
                "{}/{}", options.base_uri.trim_end_matches('/'), GRAPH_SLUG,
            )),
            triples: mapped_triples,
        });
    }

    Ok(())
}
```

### WASM in-memory path (reviewer W9: DRY — eliminate duplicate)

> **Implementation note:** The full architectural refactor (dispatching
> `export_browser_files` through `spawn_postprocessors`) was assessed as
> too risky for a single change — it would touch the in-memory path's
> core collection/serialization flow. Instead, the mapping+reasoning
> logic was extracted into a shared function
> (`ontology_mapper_producer::apply_ontology_mapping`) that both the
> postprocess plugin and the WASM in-memory path call. This eliminates
> the code duplication (W9) while keeping the change surface minimal.

The `export_browser_files` function in `runner.rs` previously had its own
inline ontology mapping logic, duplicating the plugin. This has been
eliminated — the in-memory path now calls the shared
`apply_ontology_mapping()` function, which handles both simple IRI
remapping AND OWL reasoning. Both paths (streaming via
`spawn_postprocessors` and in-memory via `apply_ontology_mapping`) now
produce identical results.

---

## CLI integration

No CLI changes needed. `spawn_postprocessors` dispatches transparently
to the plugin's `postprocess()`, which now includes reasoning. The CLI
already collects all batches before postprocess (changed in the previous
commit).

---

## Test plan

### Unit tests (owl-reasoner crate)

1. **Parser tests**
   - Parse `owl:equivalentClass` with blank node (intersection)
   - Parse `owl:equivalentClass` named↔named → **no rule generated** (W5)
   - Parse `owl:intersectionOf` with 2 and 3 elements
   - Parse `owl:unionOf` with 2 elements
   - Parse `owl:Restriction` with `someValuesFrom owl:Thing`
   - Parse `owl:Restriction` with `someValuesFrom NamedClass`
   - Parse `owl:Restriction` with `allValuesFrom` (note: not `someValuesFrom` — S2)
   - Parse `owl:Restriction` with `hasValue` (IRI and literal)
   - Parse `owl:Restriction` with `cardinality`, `minCardinality`, `maxCardinality`
   - Parse `owl:complementOf`
   - Parse nested expression (intersection containing restrictions)
   - Parse RDF list with `rdf:nil` terminator
   - Skip unsupported blank node constructs (return warning, not error)

2. **TripleIndex tests**
   - `has_type` returns true/false correctly
   - `get_objects` returns correct values
   - `subjects_of_type` returns correct subjects (O(1) via `by_type`)
   - `count_distinct_objects` deduplicates (W2)
   - Empty index edge cases

3. **Evaluator tests**
   - `Named` class matches `rdf:type`
   - `owl:Thing` always matches
   - `Intersection` — all parts must match
   - `Union` — any part matches
   - `Complement` — negation
   - `someValuesFrom owl:Thing` — has any **IRI** value (not literal — W1)
   - `someValuesFrom NamedClass` — has value typed as class
   - `allValuesFrom` — all values typed as class; vacuously true for empty
   - `hasValue` — exact value match (IRI and literal)
   - `cardinality` / `min` / `max` — count distinct values (W2)
   - Nested: intersection of Named + Restriction

4. **Candidate filtering tests (reviewer C3)**
   - `Intersection` → intersect candidate sets
   - `Union` → union candidate sets
   - `Complement` → all subjects (no pre-filter)
   - `Restriction` → subjects with the property

5. **Integration tests**
   - Full pipeline: parse alignment → build index → infer → verify output
   - The user's example:

     ```turtle
     saref4bldg:Building owl:equivalentClass ifc:IfcBuilding .
     saref4bldg:OwnedBuilding owl:equivalentClass [
         owl:intersectionOf (
             saref4bldg:Building
             [ owl:onProperty ifc:ownerHistory_IfcRoot; owl:someValuesFrom owl:Thing ]
         )
     ] .
     ```

     With input triples:

     ```
     inst:building1 rdf:type ifc:IfcBuilding .
     inst:building1 ifc:ownerHistory_IfcRoot inst:history1 .
     inst:building2 rdf:type ifc:IfcBuilding .
     ```

     After simple mapping: `building1` and `building2` both get
     `rdf:type saref4bldg:Building` (in mapped_triples).
     After reasoning: `building1` (has ownerHistory) gets
     `rdf:type saref4bldg:OwnedBuilding`; `building2` does NOT.
   - Dedup: inferred type already asserted → not duplicated (W6)
   - Parse failure: malformed expression → simple mapping still works (W8)

6. **Performance test**
   - 100K triples, 5 rules → completes in < 2 seconds
   - 2M triples, 5 rules → completes in < 30 seconds
   - Memory: index uses < 5x the triple data size

### Existing test compatibility

- All existing `ontology-mapper-producer` tests pass unchanged
- Simple mapping path (no complex expressions) produces identical output
- `ifc2lbd-wasm` and `ifc2lbd-cli` test suites pass

---

## Performance analysis

### Index construction

- **O(n)** single pass over all triples
- Memory: 3–5× raw triple data (reviewer W10). For 2M triples (~200 MB),
  index is ~600 MB–1 GB. The `needs_full_graph` path already buffers
  everything, so the additional memory is the index itself.

### Rule evaluation

- **Candidate filtering** (structurally aware — reviewer C3):
  - `Intersection` → intersect candidate sets from each part
  - `Union` → union candidate sets
  - `Complement` → all subjects (no pre-filter possible)
  - `Restriction` → subjects that have the restricted property
  - `Named` → `subjects_of_type()` via reverse index (O(1) — reviewer W3)
- **Per-subject evaluation**: O(depth × branching) of the expression tree.
  For typical alignments (intersection of 2-3 conditions), O(1) per subject.
- **Single pass**: no fixpoint iteration (reviewer W4). Documented limitation.

### WASM memory

- 4 GB cap (wasm32), ~2 GB usable. Index for 2M triples: ~600 MB–1 GB.
  Tight but feasible. **Mitigation:** cap-and-warn for large triple counts;
  skip reasoning (simple mapping still runs) if triples exceed threshold.

---

## Documented limitations (Phase 1)

| Limitation | Reason | Workaround |
|-----------|--------|------------|
| No fixpoint iteration | Single-pass evaluation | Order axioms: base `equivalentClass` before derived |
| Closed-world cardinality | Count asserted triples, not possible values | Document assumption; deduplicate values |
| `someValuesFrom owl:Thing` ignores literals | OWL DL: Thing ≠ Literal | Use `allValuesFrom xsd:string` for literal checks (Phase 2) |
| No complex `subClassOf` expressions | Different rule direction from `equivalentClass` | Use `equivalentClass` instead |
| No transitive `subClassOf` closure | Deferred to Phase 2 | Write explicit `equivalentClass` for each level |

---

## File-by-file change list

| File | Change |
|------|--------|
| `Cargo.toml` (root) | Add `owl-reasoner` to workspace members + dependencies |
| `crates/owl-reasoner/Cargo.toml` | New crate |
| `crates/owl-reasoner/src/lib.rs` | Public API: `parse_rules`, `infer_types`, `Rule`, `InferredTriple` |
| `crates/owl-reasoner/src/expression.rs` | `ClassExpression`, `Restriction`, `RestrictionKind` |
| `crates/owl-reasoner/src/parser.rs` | RDF → `ClassExpression` tree, RDF list parsing |
| `crates/owl-reasoner/src/index.rs` | `TripleIndex` with `by_type` reverse index |
| `crates/owl-reasoner/src/evaluator.rs` | `evaluate()`, `evaluate_object()`, `candidate_subjects()` |
| `crates/owl-reasoner/tests/integration.rs` | End-to-end tests including user's example |
| `crates/ontology-mapper-producer/src/lib.rs` | Use `owl_reasoner::parse_rules` + `infer_types`; apply reasoning after simple mapping |
| `crates/ontology-mapper-producer/Cargo.toml` | Add `owl-reasoner` dependency |
| `crates/ifc2lbd-wasm/src/runner.rs` | Eliminate in-memory duplicate (W9): dispatch through `spawn_postprocessors` |
| `crates/ifc2lbd-cli/src/pipeline_plugins.rs` | No change |
| `crates/ifc2lbd-cli/src/main.rs` | No change |

---

## Review findings addressed

| ID | Severity | Finding | Fix in plan |
|----|----------|---------|-------------|
| C1 | Critical | `subClassOf` semantics inverted | Removed complex `subClassOf` from scope; documented |
| C2 | Critical | Reasoning on wrong triple set | Run reasoning after simple mapping; index includes mapped triples |
| C3 | Critical | Candidate filtering unsound for union/complement | Structurally-aware `candidate_subjects()` |
| W1 | Warning | `someValuesFrom owl:Thing` matches literals | Check `Object::Iri(_)` only |
| W2 | Warning | Cardinality counts raw triples | `count_distinct_objects()` with dedup |
| W3 | Warning | No reverse index | Added `by_type: HashMap<String, HashSet<String>>` |
| W4 | Warning | No fixpoint | Documented as single-pass limitation |
| W5 | Warning | `equivalentClass` named↔named double-handled | Excluded from rule generation |
| W6 | Warning | No dedup of inferred triples | Guard with `existing_types` set |
| W7 | Warning | `apply_simple_mappings` changes output semantics | Preserve changed-only filter |
| W8 | Warning | Parse failure regresses simple mapping | `parse_rules` failure → empty rules + warning |
| W9 | Warning | DRY violation in WASM in-memory path | Eliminate duplicate; dispatch through `spawn_postprocessors` |
| W10 | Warning | Memory estimate optimistic | Updated to 3–5×; cap-and-warn mitigation |
| S1 | Suggestion | Don't bundle simple maps in reasoner | `owl-reasoner` returns only `Vec<Rule>` |
| S2 | Suggestion | Doc typo in `allValuesFrom` | Fixed |
| S3 | Suggestion | Cargo workspace references | Use `{ workspace = true }` |
| S4 | Suggestion | `InferredTriple.predicate` as constant | Store as `pub const RDF_TYPE` |
| S5 | Suggestion | `evaluate_object` literal edge case | Documented as Phase 1 limitation |
