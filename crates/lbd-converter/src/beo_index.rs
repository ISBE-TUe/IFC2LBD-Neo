//! Allowlist of classes actually declared by the Building Element Ontology (BEO).
//!
//! The converter derives BEO class names from IFC entity names and predefined
//! types, which is a guess: nothing in the IFC data says whether the resulting
//! IRI exists. Emitting an undeclared IRI is silently harmful — the triples load
//! and the counts look right, but `rdfs:subClassOf*` finds no ancestors, SHACL
//! cannot target the type, and a UI renders a raw IRI. Consulting this list
//! before emitting turns that class of bug into no triple at all.
//!
//! Two guesses need guarding, and they fail independently:
//!
//! - the **base class**, e.g. `Furniture` from `IFCFURNITURE` — BEO has no
//!   furniture concept at all, so this yields an undeclared `beo:Furniture`;
//! - the **predefined-type suffix**, e.g. `Railing-NOTDEFINED`. BEO ships the
//!   real variants (`Railing-BALUSTRADE`, `-GUARDRAIL`, `-HANDRAIL`) but not
//!   `NOTDEFINED`, which means "no subtype stated" — the base class, not a
//!   subtype called NOTDEFINED. `USERDEFINED` and any enum misread by
//!   `element_predefined_type`'s catch-all arm fail the same way.
//!
//! The list is generated from the vendored ontology by
//! `scripts/build_beo_index.py`; see `ontologies/beo.ttl` for provenance and
//! version. Regenerate both together.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Sorted BEO class local names, one per line, generated from `ontologies/beo.ttl`.
///
/// Embedded rather than parsing the 137 KB Turtle at runtime: the WASM build pays
/// for every embedded byte, and only the set of names is ever needed.
const BEO_CLASSES: &str = include_str!("../resources/beo_classes.txt");

fn declared_classes() -> &'static HashSet<&'static str> {
    static CLASSES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CLASSES.get_or_init(|| BEO_CLASSES.lines().filter(|line| !line.is_empty()).collect())
}

/// Whether BEO declares a class with this local name (e.g. `Railing`,
/// `Railing-HANDRAIL`).
pub(crate) fn beo_declares(local_name: &str) -> bool {
    declared_classes().contains(local_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_base_classes_reported_by_the_vocabulary_audit() {
        for name in [
            "Railing",
            "Stair",
            "Roof",
            "Slab",
            "BuildingElement",
        ] {
            assert!(beo_declares(name), "BEO should declare {name}");
        }
    }

    #[test]
    fn declares_real_predefined_type_variants() {
        for name in [
            "Railing-BALUSTRADE",
            "Railing-GUARDRAIL",
            "Railing-HANDRAIL",
        ] {
            assert!(beo_declares(name), "BEO should declare {name}");
        }
    }

    #[test]
    fn does_not_declare_notdefined_variants() {
        for name in [
            "Railing-NOTDEFINED",
            "Stair-NOTDEFINED",
            "Roof-NOTDEFINED",
            "Slab-NOTDEFINED",
            "BuildingElement-NOTDEFINED",
        ] {
            assert!(!beo_declares(name), "BEO must not declare {name}");
        }
    }

    /// BEO has no furniture concept — zero matches for `Furni` in the ontology.
    /// `IFCFURNISHINGELEMENT` / `IFCFURNITURE` therefore get no product-class
    /// type; they keep `bot:Element` and their ifcOWL / bSDD typing.
    #[test]
    fn does_not_declare_furniture() {
        assert!(!beo_declares("Furniture"));
    }

    #[test]
    fn does_not_declare_userdefined_variants() {
        assert!(!beo_declares("Railing-USERDEFINED"));
        assert!(!beo_declares("Wall-USERDEFINED"));
    }

    #[test]
    fn allowlist_is_populated() {
        assert!(
            declared_classes().len() >= 150,
            "allowlist looks truncated: {} entries",
            declared_classes().len()
        );
    }
}
