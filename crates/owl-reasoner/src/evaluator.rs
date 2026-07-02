//! OWL class expression evaluator.
//!
//! The core reasoning engine: evaluate whether a subject satisfies a
//! `ClassExpression` given a `TripleIndex`, and compute candidate subject
//! sets for efficient rule application.

use std::collections::HashSet;

use lbd_ontology::Object;

use crate::expression::vocab::OWL_THING;
use crate::expression::{is_owl_thing, ClassExpression, RestrictionKind};
use crate::index::TripleIndex;

/// Evaluate whether `subject` satisfies `expression` given the triple
/// index.
///
/// This is the recursive core of the reasoner. It handles all OWL construct
/// types:
/// - `Named` → check `rdf:type` (or `true` for `owl:Thing`)
/// - `Intersection` → ALL parts must match
/// - `Union` → ANY part must match
/// - `Complement` → NOT inner
/// - `Restriction` → depends on restriction kind
pub fn evaluate(subject: &str, expression: &ClassExpression, index: &TripleIndex) -> bool {
    match expression {
        ClassExpression::Named(iri) => {
            if iri == OWL_THING {
                true
            } else {
                index.has_type(subject, iri)
            }
        }

        ClassExpression::Intersection(parts) => parts.iter().all(|p| evaluate(subject, p, index)),

        ClassExpression::Union(parts) => parts.iter().any(|p| evaluate(subject, p, index)),

        ClassExpression::Complement(inner) => !evaluate(subject, inner, index),

        ClassExpression::Restriction(r) => match &r.kind {
            RestrictionKind::SomeValuesFrom(class_expr) => {
                let objects = index.get_objects(subject, &r.property);
                if is_owl_thing(class_expr) {
                    // owl:Thing matches IRI values only, NOT literals.
                    // (OWL DL: Thing ≠ Literal)
                    objects.iter().any(|obj| matches!(obj, Object::Iri(_)))
                } else {
                    objects
                        .iter()
                        .any(|obj| evaluate_object(obj, class_expr, index))
                }
            }

            RestrictionKind::AllValuesFrom(class_expr) => {
                let objects = index.get_objects(subject, &r.property);
                if objects.is_empty() {
                    // Vacuously true: no values means all values satisfy
                    true
                } else if is_owl_thing(class_expr) {
                    // owl:Thing: all values must be IRIs (not literals)
                    objects.iter().all(|obj| matches!(obj, Object::Iri(_)))
                } else {
                    objects
                        .iter()
                        .all(|obj| evaluate_object(obj, class_expr, index))
                }
            }

            RestrictionKind::HasValue(value) => index
                .get_objects(subject, &r.property)
                .iter()
                .any(|obj| obj == value),

            RestrictionKind::ExactCardinality(n) => {
                index.count_distinct_objects(subject, &r.property) == *n
            }

            RestrictionKind::MinCardinality(n) => {
                index.count_distinct_objects(subject, &r.property) >= *n
            }

            RestrictionKind::MaxCardinality(n) => {
                index.count_distinct_objects(subject, &r.property) <= *n
            }
        },
    }
}

/// Evaluate whether an `Object` (IRI or literal) satisfies a class
/// expression.
///
/// - IRI values are recursively evaluated as subjects against the index.
/// - Literal values can only satisfy `owl:Thing` at the top level.
///   Complex datatype restrictions are out of scope (Phase 1).
fn evaluate_object(obj: &Object, expression: &ClassExpression, index: &TripleIndex) -> bool {
    match obj {
        Object::Iri(iri) => evaluate(iri, expression, index),
        Object::Literal(_) | Object::TypedLiteral { .. } => {
            // Literals can only satisfy owl:Thing.
            is_owl_thing(expression)
        }
    }
}

/// Compute the candidate subject set for a rule's condition expression.
///
/// This is a structurally-aware filter that narrows the subjects to
/// evaluate before running the full (recursive) evaluation:
/// - `Named` → `subjects_of_type()` via reverse index (O(1))
/// - `Intersection` → intersect candidate sets from each part
/// - `Union` → union candidate sets from each part
/// - `Complement` → ALL subjects (can't pre-filter a negation)
/// - `Restriction` → subjects that have the restricted property (IRI-valued
///   for someValuesFrom/hasValue, any-valued for allValuesFrom/cardinality)
///
/// This is a **filter**, not a complete evaluator. After candidate
/// filtering, the full `evaluate()` is run on each candidate to confirm
/// the match.
pub fn candidate_subjects(
    expression: &ClassExpression,
    index: &TripleIndex,
    all_subjects: &[String],
) -> Vec<String> {
    match expression {
        ClassExpression::Named(iri) => {
            if iri == OWL_THING {
                // owl:Thing matches all subjects — return all
                all_subjects.to_vec()
            } else {
                index
                    .subjects_of_type(iri)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default()
            }
        }

        ClassExpression::Intersection(parts) => {
            if parts.is_empty() {
                return Vec::new();
            }
            // Start with the first part's candidates, then intersect with
            // each subsequent part.
            let mut result: HashSet<String> = candidate_subjects(&parts[0], index, all_subjects)
                .into_iter()
                .collect();
            for part in &parts[1..] {
                let next: HashSet<String> = candidate_subjects(part, index, all_subjects)
                    .into_iter()
                    .collect();
                result.retain(|s| next.contains(s));
                if result.is_empty() {
                    break; // Early exit: empty intersection
                }
            }
            result.into_iter().collect()
        }

        ClassExpression::Union(parts) => {
            let mut result = HashSet::new();
            for part in parts {
                for s in candidate_subjects(part, index, all_subjects) {
                    result.insert(s);
                }
            }
            result.into_iter().collect()
        }

        ClassExpression::Complement(_) => {
            // Can't pre-filter negation — must evaluate all subjects
            all_subjects.to_vec()
        }

        ClassExpression::Restriction(r) => {
            // Filter to subjects that have the restricted property.
            // For someValuesFrom with owl:Thing, filter to IRI-valued
            // properties. For hasValue, any property presence. For
            // allValuesFrom/cardinality, any property presence.
            all_subjects
                .iter()
                .filter(|s| {
                    match &r.kind {
                        RestrictionKind::SomeValuesFrom(expr) => {
                            if is_owl_thing(expr) {
                                index.has_iri_property(s, &r.property)
                            } else {
                                !index.get_objects(s, &r.property).is_empty()
                            }
                        }
                        RestrictionKind::AllValuesFrom(_) => {
                            // Even subjects with 0 values satisfy
                            // allValuesFrom (vacuous truth), but filtering
                            // them out is safe — the full evaluator will
                            // re-add vacuous truths.  However, to be
                            // conservative, we only filter to subjects
                            // WITH the property.  This is correct because:
                            // if a subject has no values for P, then
                            // allValuesFrom is vacuously true, but the
                            // Intersection with other parts (e.g. Named
                            // class) will still include it via those parts.
                            // If allValuesFrom is the ONLY condition, we
                            // still need those subjects — so we return
                            // all subjects here.
                            true
                        }
                        RestrictionKind::HasValue(_) => {
                            !index.get_objects(s, &r.property).is_empty()
                        }
                        RestrictionKind::ExactCardinality(_)
                        | RestrictionKind::MinCardinality(_)
                        | RestrictionKind::MaxCardinality(_) => {
                            // Cardinality 0 is possible (maxCardinality 0),
                            // so we can't filter by property presence.
                            true
                        }
                    }
                })
                .cloned()
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::Restriction;
    use crate::index::TripleIndex;
    use lbd_ontology::Triple;

    fn make_triple(s: &str, p: &str, o: Object) -> Triple {
        Triple {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o,
        }
    }

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

    fn build_index(triples: Vec<Triple>) -> TripleIndex {
        TripleIndex::from_triples(&triples)
    }

    // --- Named class ---

    #[test]
    fn test_named_class_matches() {
        let idx = build_index(vec![make_triple(
            "http://ex.org/wall1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        )]);
        assert!(evaluate(
            "http://ex.org/wall1",
            &ClassExpression::Named("http://ex.org/Wall".to_string()),
            &idx
        ));
        assert!(!evaluate(
            "http://ex.org/wall1",
            &ClassExpression::Named("http://ex.org/Slab".to_string()),
            &idx
        ));
    }

    #[test]
    fn test_owl_thing_always_matches() {
        let idx = build_index(vec![make_triple(
            "http://ex.org/wall1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        )]);
        assert!(evaluate(
            "http://ex.org/wall1",
            &ClassExpression::Named("http://www.w3.org/2002/07/owl#Thing".to_string()),
            &idx
        ));
        // Even a subject with no triples matches owl:Thing
        assert!(evaluate(
            "http://ex.org/unknown",
            &ClassExpression::Named("http://www.w3.org/2002/07/owl#Thing".to_string()),
            &idx
        ));
    }

    // --- Intersection ---

    #[test]
    fn test_intersection_all_must_match() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/isExternal",
                Object::Iri("http://ex.org/val".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
        ]);
        let expr = ClassExpression::Intersection(vec![
            ClassExpression::Named("http://ex.org/Wall".to_string()),
            ClassExpression::Restriction(Box::new(Restriction {
                property: "http://ex.org/isExternal".to_string(),
                kind: RestrictionKind::SomeValuesFrom(ClassExpression::Named(
                    "http://www.w3.org/2002/07/owl#Thing".to_string(),
                )),
            })),
        ]);
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        assert!(!evaluate("http://ex.org/b", &expr, &idx));
    }

    // --- Union ---

    #[test]
    fn test_union_any_must_match() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Door".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Window".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
        ]);
        let expr = ClassExpression::Union(vec![
            ClassExpression::Named("http://ex.org/Door".to_string()),
            ClassExpression::Named("http://ex.org/Window".to_string()),
        ]);
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        assert!(evaluate("http://ex.org/b", &expr, &idx));
        assert!(!evaluate("http://ex.org/c", &expr, &idx));
    }

    // --- Complement ---

    #[test]
    fn test_complement_negation() {
        let idx = build_index(vec![make_triple(
            "http://ex.org/a",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        )]);
        let expr = ClassExpression::Complement(Box::new(ClassExpression::Named(
            "http://ex.org/Wall".to_string(),
        )));
        assert!(!evaluate("http://ex.org/a", &expr, &idx));
        assert!(evaluate("http://ex.org/b", &expr, &idx));
    }

    // --- someValuesFrom ---

    #[test]
    fn test_some_values_from_owl_thing_iri_only() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasRef",
                Object::Iri("http://ex.org/b".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                "http://ex.org/hasName",
                Object::Literal("foo".to_string()),
            ),
        ]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasRef".to_string(),
            kind: RestrictionKind::SomeValuesFrom(ClassExpression::Named(
                "http://www.w3.org/2002/07/owl#Thing".to_string(),
            )),
        }));
        // 'a' has an IRI value → matches
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        // 'c' has only a literal value → does NOT match
        assert!(!evaluate("http://ex.org/c", &expr, &idx));
    }

    #[test]
    fn test_some_values_from_named_class() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/hasMaterial",
                Object::Iri("http://ex.org/mat1".to_string()),
            ),
            make_triple(
                "http://ex.org/mat1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wood".to_string()),
            ),
            make_triple(
                "http://ex.org/wall2",
                "http://ex.org/hasMaterial",
                Object::Iri("http://ex.org/mat2".to_string()),
            ),
            make_triple(
                "http://ex.org/mat2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Concrete".to_string()),
            ),
        ]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasMaterial".to_string(),
            kind: RestrictionKind::SomeValuesFrom(ClassExpression::Named(
                "http://ex.org/Wood".to_string(),
            )),
        }));
        assert!(evaluate("http://ex.org/wall1", &expr, &idx));
        assert!(!evaluate("http://ex.org/wall2", &expr, &idx));
    }

    #[test]
    fn test_some_values_from_nested() {
        // OPM-style: wall → props:isExternal → property_node → opm:hasPropertyState → state → schema:value → "true"
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/hasProp",
                Object::Iri("http://ex.org/prop1".to_string()),
            ),
            make_triple(
                "http://ex.org/prop1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Property".to_string()),
            ),
            make_triple(
                "http://ex.org/prop1",
                "http://ex.org/hasState",
                Object::Iri("http://ex.org/state1".to_string()),
            ),
            make_triple(
                "http://ex.org/state1",
                RDF_TYPE,
                Object::Iri("http://ex.org/CurrentState".to_string()),
            ),
            make_triple(
                "http://ex.org/state1",
                "http://ex.org/value",
                Object::TypedLiteral {
                    value: "true".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
            // wall2 without isExternal=true
            make_triple(
                "http://ex.org/wall2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall2",
                "http://ex.org/hasProp",
                Object::Iri("http://ex.org/prop2".to_string()),
            ),
            make_triple(
                "http://ex.org/prop2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Property".to_string()),
            ),
            make_triple(
                "http://ex.org/prop2",
                "http://ex.org/hasState",
                Object::Iri("http://ex.org/state2".to_string()),
            ),
            make_triple(
                "http://ex.org/state2",
                RDF_TYPE,
                Object::Iri("http://ex.org/CurrentState".to_string()),
            ),
            make_triple(
                "http://ex.org/state2",
                "http://ex.org/value",
                Object::TypedLiteral {
                    value: "false".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
        ]);

        // 3-level nested someValuesFrom (OPM property reification)
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasProp".to_string(),
            kind: RestrictionKind::SomeValuesFrom(ClassExpression::Intersection(vec![
                ClassExpression::Named("http://ex.org/Property".to_string()),
                ClassExpression::Restriction(Box::new(Restriction {
                    property: "http://ex.org/hasState".to_string(),
                    kind: RestrictionKind::SomeValuesFrom(ClassExpression::Intersection(vec![
                        ClassExpression::Named("http://ex.org/CurrentState".to_string()),
                        ClassExpression::Restriction(Box::new(Restriction {
                            property: "http://ex.org/value".to_string(),
                            kind: RestrictionKind::HasValue(Object::TypedLiteral {
                                value: "true".to_string(),
                                datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                            }),
                        })),
                    ])),
                })),
            ])),
        }));

        assert!(evaluate("http://ex.org/wall1", &expr, &idx));
        assert!(!evaluate("http://ex.org/wall2", &expr, &idx));
    }

    // --- allValuesFrom ---

    #[test]
    fn test_all_values_from() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasMaterial",
                Object::Iri("http://ex.org/m1".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasMaterial",
                Object::Iri("http://ex.org/m2".to_string()),
            ),
            make_triple(
                "http://ex.org/m1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wood".to_string()),
            ),
            make_triple(
                "http://ex.org/m2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wood".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                "http://ex.org/hasMaterial",
                Object::Iri("http://ex.org/m3".to_string()),
            ),
            make_triple(
                "http://ex.org/m3",
                RDF_TYPE,
                Object::Iri("http://ex.org/Concrete".to_string()),
            ),
        ]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasMaterial".to_string(),
            kind: RestrictionKind::AllValuesFrom(ClassExpression::Named(
                "http://ex.org/Wood".to_string(),
            )),
        }));
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        assert!(!evaluate("http://ex.org/b", &expr, &idx));
    }

    #[test]
    fn test_all_values_from_vacuous_truth() {
        // No values → vacuously true
        let idx = build_index(vec![]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasMaterial".to_string(),
            kind: RestrictionKind::AllValuesFrom(ClassExpression::Named(
                "http://ex.org/Wood".to_string(),
            )),
        }));
        assert!(evaluate("http://ex.org/a", &expr, &idx));
    }

    // --- hasValue ---

    #[test]
    fn test_has_value_iri() {
        let idx = build_index(vec![make_triple(
            "http://ex.org/a",
            "http://ex.org/hasOwner",
            Object::Iri("http://ex.org/acme".to_string()),
        )]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasOwner".to_string(),
            kind: RestrictionKind::HasValue(Object::Iri("http://ex.org/acme".to_string())),
        }));
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        assert!(!evaluate("http://ex.org/b", &expr, &idx));
    }

    #[test]
    fn test_has_value_literal_typed() {
        let idx = build_index(vec![make_triple(
            "http://ex.org/a",
            "http://ex.org/isExternal",
            Object::TypedLiteral {
                value: "true".to_string(),
                datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
            },
        )]);
        let expr = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/isExternal".to_string(),
            kind: RestrictionKind::HasValue(Object::TypedLiteral {
                value: "true".to_string(),
                datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
            }),
        }));
        assert!(evaluate("http://ex.org/a", &expr, &idx));
        // Different datatype → no match
        let expr_wrong_dt = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/isExternal".to_string(),
            kind: RestrictionKind::HasValue(Object::Literal("true".to_string())),
        }));
        assert!(!evaluate("http://ex.org/a", &expr_wrong_dt, &idx));
    }

    // --- Cardinality ---

    #[test]
    fn test_exact_cardinality() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d1".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d2".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d3".to_string()),
            ),
        ]);
        let expr = |n: usize| {
            ClassExpression::Restriction(Box::new(Restriction {
                property: "http://ex.org/hasDoor".to_string(),
                kind: RestrictionKind::ExactCardinality(n),
            }))
        };
        assert!(evaluate("http://ex.org/a", &expr(2), &idx));
        assert!(!evaluate("http://ex.org/a", &expr(1), &idx));
        assert!(evaluate("http://ex.org/b", &expr(1), &idx));
    }

    #[test]
    fn test_min_max_cardinality() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d1".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d2".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d1".to_string()),
            ), // duplicate
        ]);
        let min1 = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasDoor".to_string(),
            kind: RestrictionKind::MinCardinality(1),
        }));
        let min3 = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasDoor".to_string(),
            kind: RestrictionKind::MinCardinality(3),
        }));
        let max2 = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasDoor".to_string(),
            kind: RestrictionKind::MaxCardinality(2),
        }));
        let max1 = ClassExpression::Restriction(Box::new(Restriction {
            property: "http://ex.org/hasDoor".to_string(),
            kind: RestrictionKind::MaxCardinality(1),
        }));
        // 2 distinct doors (d1, d2) — duplicate deduped
        assert!(evaluate("http://ex.org/a", &min1, &idx));
        assert!(!evaluate("http://ex.org/a", &min3, &idx));
        assert!(evaluate("http://ex.org/a", &max2, &idx));
        assert!(!evaluate("http://ex.org/a", &max1, &idx));
    }

    // --- Candidate filtering ---

    #[test]
    fn test_candidate_subjects_named() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                RDF_TYPE,
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
        ]);
        let candidates = candidate_subjects(
            &ClassExpression::Named("http://ex.org/Wall".to_string()),
            &idx,
            idx.all_subjects(),
        );
        let set: HashSet<String> = candidates.into_iter().collect();
        assert_eq!(set.len(), 2);
        assert!(set.contains("http://ex.org/a"));
        assert!(set.contains("http://ex.org/b"));
    }

    #[test]
    fn test_candidate_subjects_intersection() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/isExternal",
                Object::Iri("http://ex.org/val".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                "http://ex.org/isExternal",
                Object::Iri("http://ex.org/val".to_string()),
            ),
        ]);
        let expr = ClassExpression::Intersection(vec![
            ClassExpression::Named("http://ex.org/Wall".to_string()),
            ClassExpression::Restriction(Box::new(Restriction {
                property: "http://ex.org/isExternal".to_string(),
                kind: RestrictionKind::SomeValuesFrom(ClassExpression::Named(
                    "http://www.w3.org/2002/07/owl#Thing".to_string(),
                )),
            })),
        ]);
        let candidates = candidate_subjects(&expr, &idx, idx.all_subjects());
        let set: HashSet<String> = candidates.into_iter().collect();
        // Only 'a' is both a Wall AND has isExternal
        assert_eq!(set.len(), 1);
        assert!(set.contains("http://ex.org/a"));
    }

    #[test]
    fn test_candidate_subjects_union() {
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Door".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Window".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
        ]);
        let expr = ClassExpression::Union(vec![
            ClassExpression::Named("http://ex.org/Door".to_string()),
            ClassExpression::Named("http://ex.org/Window".to_string()),
        ]);
        let candidates = candidate_subjects(&expr, &idx, idx.all_subjects());
        let set: HashSet<String> = candidates.into_iter().collect();
        assert_eq!(set.len(), 2);
        assert!(set.contains("http://ex.org/a"));
        assert!(set.contains("http://ex.org/b"));
    }

    #[test]
    fn test_candidate_subjects_complement() {
        // Complement → all subjects (can't pre-filter)
        let idx = build_index(vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
        ]);
        let expr = ClassExpression::Complement(Box::new(ClassExpression::Named(
            "http://ex.org/Wall".to_string(),
        )));
        let candidates = candidate_subjects(&expr, &idx, idx.all_subjects());
        assert_eq!(candidates.len(), 2); // all subjects
    }
}
