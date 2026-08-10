//! Which quantity names are standard IFC base quantities.
//!
//! Real exports are full of exporter-specific quantities — ArchiCAD's
//! `Oberkante zu Meereshöhe`, Revit's per-family `AC_Equantity_*` sets. They are
//! legitimate data, but they are not geometry any QTO backend should compute, and
//! counting them as coverage misses drowns the number that matters. One German
//! model in the corpus contributes 150,165 quantities of which none are standard.
//!
//! Set names cannot be used for this: that same model puts genuine base
//! quantities in ArchiCAD's `BaseQuantities` rather than a `Qto_*` set. So the
//! test is on the quantity *name*, against the authoritative list extracted from
//! the vendored bSDD IFC4x3 index.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Generated from `crates/lbd-converter/resources/bsdd_ifc4x3_index.json.gz` —
/// every distinct property name appearing in any `Qto_*` set.
const STANDARD: &str = include_str!("../resources/standard_quantity_names.txt");

fn standard_set() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        STANDARD
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_ascii_lowercase())
            .collect()
    })
}

/// Case-insensitive because exporters disagree on capitalisation — notably
/// `GrossFootprintArea` (IFC4) vs bSDD IFC4x3's `GrossFootPrintArea`.
pub fn is_standard_quantity(name: &str) -> bool {
    standard_set().contains(&name.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_standard_quantities() {
        for n in ["Length", "NetVolume", "GrossSideArea", "CrossSectionArea"] {
            assert!(is_standard_quantity(n), "{n} should be standard");
        }
    }

    #[test]
    fn both_footprint_spellings_are_recognised() {
        assert!(is_standard_quantity("GrossFootPrintArea"));
        assert!(is_standard_quantity("GrossFootprintArea"));
    }

    #[test]
    fn rejects_exporter_specific_quantities() {
        for n in ["Oberkante zu Meereshöhe", "Höhe zu 1. Referenzhöhe", "Quantity"] {
            assert!(!is_standard_quantity(n), "{n} should not be standard");
        }
    }
}
