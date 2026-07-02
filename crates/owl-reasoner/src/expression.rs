//! OWL class expression data model.
//!
//! An OWL class expression is a tree of conditions that a subject must
//! satisfy. The reasoner parses RDF/Turtle axioms (e.g.
//! `owl:equivalentClass` with blank-node right side) into this enum and
//! evaluates it against the triple index.

use lbd_ontology::Object;

/// An OWL class expression — a tree of conditions a subject must satisfy.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassExpression {
    /// A named class IRI (e.g. `saref4bldg:Building`).
    Named(String),

    /// `owl:intersectionOf` — subject must satisfy ALL expressions.
    Intersection(Vec<ClassExpression>),

    /// `owl:unionOf` — subject must satisfy at least ONE expression.
    Union(Vec<ClassExpression>),

    /// `owl:complementOf` — subject must NOT satisfy the expression.
    Complement(Box<ClassExpression>),

    /// `owl:Restriction` — a property restriction.
    Restriction(Box<Restriction>),
}

/// An OWL restriction on a property.
#[derive(Clone, Debug, PartialEq)]
pub struct Restriction {
    /// The property IRI (`owl:onProperty`).
    pub property: String,
    /// The kind of restriction.
    pub kind: RestrictionKind,
}

/// The kind of OWL property restriction.
#[derive(Clone, Debug, PartialEq)]
pub enum RestrictionKind {
    /// `owl:someValuesFrom` — subject has ≥1 value whose type satisfies
    /// `class`. `owl:Thing` is represented as
    /// `ClassExpression::Named(OWL_THING)`.
    SomeValuesFrom(ClassExpression),

    /// `owl:allValuesFrom` — subject's ALL values satisfy `class`.
    AllValuesFrom(ClassExpression),

    /// `owl:hasValue` — subject has a specific value (IRI or literal).
    HasValue(Object),

    /// `owl:cardinality` — exactly N distinct values (closed-world).
    ExactCardinality(usize),

    /// `owl:minCardinality` — at least N distinct values (closed-world).
    MinCardinality(usize),

    /// `owl:maxCardinality` — at most N distinct values (closed-world).
    MaxCardinality(usize),
}

/// Well-known IRI constants used throughout the reasoner.
pub mod vocab {
    pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    pub const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
    pub const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
    pub const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

    pub const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
    pub const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
    pub const OWL_RESTRICTION: &str = "http://www.w3.org/2002/07/owl#Restriction";
    pub const OWL_ON_PROPERTY: &str = "http://www.w3.org/2002/07/owl#onProperty";
    pub const OWL_SOME_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#someValuesFrom";
    pub const OWL_ALL_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#allValuesFrom";
    pub const OWL_HAS_VALUE: &str = "http://www.w3.org/2002/07/owl#hasValue";
    pub const OWL_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#cardinality";
    pub const OWL_MIN_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#minCardinality";
    pub const OWL_MAX_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#maxCardinality";
    pub const OWL_INTERSECTION_OF: &str = "http://www.w3.org/2002/07/owl#intersectionOf";
    pub const OWL_UNION_OF: &str = "http://www.w3.org/2002/07/owl#unionOf";
    pub const OWL_COMPLEMENT_OF: &str = "http://www.w3.org/2002/07/owl#complementOf";
    pub const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";

    pub const XSD_NON_NEGATIVE_INTEGER: &str =
        "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";
}

use vocab::{OWL_CLASS, OWL_RESTRICTION, OWL_THING};

/// Check whether a class expression is exactly `owl:Thing`.
pub fn is_owl_thing(expr: &ClassExpression) -> bool {
    matches!(expr, ClassExpression::Named(iri) if iri == OWL_THING)
}

/// Format a class expression for error messages / debugging.
impl std::fmt::Display for ClassExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassExpression::Named(iri) => write!(f, "<{iri}>"),
            ClassExpression::Intersection(parts) => {
                write!(f, "intersectionOf(")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ")")
            }
            ClassExpression::Union(parts) => {
                write!(f, "unionOf(")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ")")
            }
            ClassExpression::Complement(inner) => write!(f, "complementOf({inner})"),
            ClassExpression::Restriction(r) => {
                write!(f, "Restriction(onProperty=<{}, ", r.property)?;
                match &r.kind {
                    RestrictionKind::SomeValuesFrom(c) => write!(f, "someValuesFrom={c})"),
                    RestrictionKind::AllValuesFrom(c) => write!(f, "allValuesFrom={c})"),
                    RestrictionKind::HasValue(v) => write!(f, "hasValue={v:?})"),
                    RestrictionKind::ExactCardinality(n) => write!(f, "cardinality={n})"),
                    RestrictionKind::MinCardinality(n) => write!(f, "minCardinality={n})"),
                    RestrictionKind::MaxCardinality(n) => write!(f, "maxCardinality={n})"),
                }
            }
        }
    }
}

/// Helper: check if an expression is a named class (used in candidate filtering).
pub fn is_named(expr: &ClassExpression) -> Option<&str> {
    match expr {
        ClassExpression::Named(iri) => Some(iri),
        _ => None,
    }
}

/// Check if a blank node identifier matches `OWL_CLASS` or `OWL_RESTRICTION`
/// (used during parsing to determine what kind of expression a blank node
/// represents).
pub fn is_owl_class_or_restriction(type_iris: &[&str]) -> Option<BlankNodeKind> {
    for &t in type_iris {
        if t == OWL_RESTRICTION {
            return Some(BlankNodeKind::Restriction);
        }
        if t == OWL_CLASS {
            return Some(BlankNodeKind::Class);
        }
    }
    None
}

/// What kind of OWL construct a blank node represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlankNodeKind {
    Class,
    Restriction,
}
