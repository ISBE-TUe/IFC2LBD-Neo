//! Last check before a computed quantity is allowed into the model.
//!
//! The governing rule for this module is that a wrong quantity is worse than a
//! missing one: downstream consumers calculate with these numbers, and a value
//! that is quietly 3% off is more dangerous than one that is obviously absent.
//! So everything here **drops and logs** rather than correcting or guessing.
//!
//! Two distinct jobs, in order:
//!
//! 1. **Unit conversion** — the compute tiers work in raw geometry units
//!    (`LENGTHUNIT`ⁿ) but quantities must be expressed in the separately declared
//!    `AREAUNIT` / `VOLUMEUNIT`. This is a conversion, not a check: the value is
//!    right, its scale is not. Where the scale cannot be established at all
//!    (conversion-based/imperial units), every quantity for that model is
//!    withheld.
//!
//! 2. **Consistency checks** — only relations that need no external reference.
//!    Deliberately *not* included: comparison against a bounding box. The
//!    existing `bbox::compute` unions points from unrelated coordinate frames and
//!    is one of the things under replacement; gating against it would launder a
//!    broken measurement into an authority.

use crate::qto_names::QuantityKind;
use crate::units::{Dimension, UnitScales};

/// Why a computed value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// NaN or infinity — a degenerate computation.
    NotFinite,
    /// Zero or negative where the quantity is necessarily positive.
    NonPositive,
    /// `NetVolume` exceeded `GrossVolume`; subtracting openings cannot add volume.
    NetExceedsGross,
    /// The model's units could not be resolved, so no scale is trustworthy.
    UnresolvableUnits,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFinite => "not-finite",
            Self::NonPositive => "non-positive",
            Self::NetExceedsGross => "net-exceeds-gross",
            Self::UnresolvableUnits => "unresolvable-units",
        }
    }
}

/// Map a quantity kind onto the physical dimension that determines its scaling.
///
/// Derived from the IFC quantity entity the kind serialises to, so it cannot
/// drift from what is actually written into the model.
pub fn dimension_of(kind: QuantityKind) -> Dimension {
    Dimension::from_quantity_entity(kind.ifc_entity_name())
}

/// Convert one raw geometric value into the unit the model declares for it.
pub fn to_declared_unit(raw: f64, kind: QuantityKind, scales: &UnitScales) -> f64 {
    let dim = dimension_of(kind);
    // geometry (LENGTHUNIT^n) → SI → declared quantity unit
    let si = scales.geometry_to_si(raw, dim);
    match dim {
        Dimension::Length => si / scales.length,
        Dimension::Area => si / scales.area,
        Dimension::Volume => si / scales.volume,
        Dimension::Other => raw,
    }
}

/// Check one value in isolation.
pub fn check_value(value: f64) -> Result<(), Rejection> {
    if !value.is_finite() {
        return Err(Rejection::NotFinite);
    }
    // Every quantity this module computes is a length, area or volume; none can
    // legitimately be zero or negative for a real element.
    if value <= 0.0 {
        return Err(Rejection::NonPositive);
    }
    Ok(())
}

/// Check relations between an element's quantities.
///
/// Returns the kinds that must be dropped. Only `NetVolume` vs `GrossVolume` is
/// checked today: it is the one relation that holds unconditionally regardless of
/// element type or measurement convention. Adding more requires being certain the
/// relation is universal — a check that is wrong for some element type would
/// suppress correct data, which is its own failure.
pub fn check_consistency(values: &[(QuantityKind, f64)]) -> Vec<(QuantityKind, Rejection)> {
    let find = |k: QuantityKind| values.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v);

    let mut rejected = Vec::new();
    if let (Some(net), Some(gross)) = (
        find(QuantityKind::NetVolume),
        find(QuantityKind::GrossVolume),
    ) {
        // Tolerance covers f64 round-off in the two independent computations,
        // not genuine disagreement.
        if net > gross * (1.0 + 1e-9) {
            rejected.push((QuantityKind::NetVolume, Rejection::NetExceedsGross));
        }
    }
    rejected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mm_geometry_si_quantities() -> UnitScales {
        // The real model C / model E case.
        UnitScales {
            length: 1e-3,
            area: 1.0,
            volume: 1.0,
        }
    }

    #[test]
    fn volume_in_mm_geometry_converts_to_declared_cubic_metres() {
        let s = mm_geometry_si_quantities();
        // 1 m³ expressed in mm³ is 1e9.
        let v = to_declared_unit(1e9, QuantityKind::GrossVolume, &s);
        assert!((v - 1.0).abs() < 1e-6, "got {v}");
    }

    #[test]
    fn area_in_mm_geometry_converts_to_declared_square_metres() {
        let s = mm_geometry_si_quantities();
        let a = to_declared_unit(1e6, QuantityKind::GrossArea, &s);
        assert!((a - 1.0).abs() < 1e-9, "got {a}");
    }

    #[test]
    fn lengths_stay_in_the_length_unit_they_were_computed_in() {
        let s = mm_geometry_si_quantities();
        // Length quantities are declared in LENGTHUNIT, the same unit the
        // geometry uses, so they must pass through untouched.
        let l = to_declared_unit(5000.0, QuantityKind::Length, &s);
        assert!((l - 5000.0).abs() < 1e-9, "got {l}");
    }

    #[test]
    fn consistent_metre_model_is_a_no_op() {
        let s = UnitScales::default();
        assert_eq!(to_declared_unit(7.2, QuantityKind::GrossVolume, &s), 7.2);
    }

    #[test]
    fn rejects_degenerate_values() {
        assert_eq!(check_value(f64::NAN), Err(Rejection::NotFinite));
        assert_eq!(check_value(f64::INFINITY), Err(Rejection::NotFinite));
        assert_eq!(check_value(0.0), Err(Rejection::NonPositive));
        assert_eq!(check_value(-1.0), Err(Rejection::NonPositive));
        assert!(check_value(0.001).is_ok());
    }

    #[test]
    fn net_volume_may_not_exceed_gross() {
        let bad = [
            (QuantityKind::GrossVolume, 1.0),
            (QuantityKind::NetVolume, 1.5),
        ];
        assert_eq!(
            check_consistency(&bad),
            vec![(QuantityKind::NetVolume, Rejection::NetExceedsGross)]
        );

        let good = [
            (QuantityKind::GrossVolume, 1.0),
            (QuantityKind::NetVolume, 0.8),
        ];
        assert!(check_consistency(&good).is_empty());

        // Equal is legitimate: an element with no openings.
        let equal = [
            (QuantityKind::GrossVolume, 1.0),
            (QuantityKind::NetVolume, 1.0),
        ];
        assert!(check_consistency(&equal).is_empty());
    }
}
