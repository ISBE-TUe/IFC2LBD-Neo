//! Which quantities each IFC class is defined to carry.
//!
//! Sourced from the vendored bSDD IFC4x3 index rather than hand-maintained; see
//! `scripts/build_qto_index.py`.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuantityKind {
    Length,
    Height,
    Width,
    Depth,
    GrossVolume,
    NetVolume,
    GrossArea,
    NetArea,
    Area,
    GrossFootprintArea,
    NetFootprintArea,
    GrossSideArea,
    NetSideArea,
    GrossFloorArea,
    NetFloorArea,
    CrossSectionArea,
    OuterSurfaceArea,
    GrossSurfaceArea,
    NetSurfaceArea,
    GrossPerimeter,
    NetPerimeter,
    Perimeter,
    Thickness,
    Diameter,
    InnerDiameter,
    OuterDiameter,
    GrossHeight,
    NetHeight,
    EavesHeight,
    FinishCeilingHeight,
    FinishFloorHeight,
    PlanLength,
    Volume,
    GrossCrossSectionArea,
    NetCrossSectionArea,
    GrossCeilingArea,
    NetCeilingArea,
    GrossWallArea,
    NetWallArea,
    FootprintArea,
    TotalSurfaceArea,
    ProjectedArea,
    PlanArea,
    SignArea,
}

impl QuantityKind {
    /// IFC quantity entity name in uppercase STEP form, e.g. "IFCQUANTITYAREA".
    pub fn ifc_entity_name(self) -> &'static str {
        match self {
            Self::Length
            | Self::Height
            | Self::Width
            | Self::Depth
            | Self::GrossPerimeter
            | Self::NetPerimeter
            | Self::Perimeter
            | Self::Thickness
            | Self::Diameter
            | Self::InnerDiameter
            | Self::OuterDiameter
            | Self::GrossHeight
            | Self::NetHeight
            | Self::EavesHeight
            | Self::FinishCeilingHeight
            | Self::FinishFloorHeight
            | Self::PlanLength => "IFCQUANTITYLENGTH",

            Self::GrossVolume | Self::NetVolume | Self::Volume => "IFCQUANTITYVOLUME",

            Self::GrossArea
            | Self::NetArea
            | Self::Area
            | Self::GrossFootprintArea
            | Self::NetFootprintArea
            | Self::GrossSideArea
            | Self::NetSideArea
            | Self::GrossFloorArea
            | Self::NetFloorArea
            | Self::CrossSectionArea
            | Self::GrossCrossSectionArea
            | Self::NetCrossSectionArea
            | Self::GrossCeilingArea
            | Self::NetCeilingArea
            | Self::GrossWallArea
            | Self::NetWallArea
            | Self::FootprintArea
            | Self::TotalSurfaceArea
            | Self::ProjectedArea
            | Self::PlanArea
            | Self::SignArea
            | Self::GrossSurfaceArea
            | Self::NetSurfaceArea
            | Self::OuterSurfaceArea => "IFCQUANTITYAREA",
        }
    }

    /// Standard IFC quantity name as it appears in STEP and as an RDF label.
    pub fn ifc_name(self) -> &'static str {
        match self {
            Self::Length => "Length",
            Self::Height => "Height",
            Self::Width => "Width",
            Self::Depth => "Depth",
            Self::GrossVolume => "GrossVolume",
            Self::NetVolume => "NetVolume",
            Self::GrossArea => "GrossArea",
            Self::NetArea => "NetArea",
            Self::Area => "Area",
            Self::GrossFootprintArea => "GrossFootprintArea",
            Self::NetFootprintArea => "NetFootprintArea",
            Self::GrossSideArea => "GrossSideArea",
            Self::NetSideArea => "NetSideArea",
            Self::GrossFloorArea => "GrossFloorArea",
            Self::NetFloorArea => "NetFloorArea",
            Self::CrossSectionArea => "CrossSectionArea",
            Self::OuterSurfaceArea => "OuterSurfaceArea",
            Self::GrossSurfaceArea => "GrossSurfaceArea",
            Self::NetSurfaceArea => "NetSurfaceArea",
            Self::GrossPerimeter => "GrossPerimeter",
            Self::NetPerimeter => "NetPerimeter",
            Self::Perimeter => "Perimeter",
            Self::Thickness => "Thickness",
            Self::Diameter => "Diameter",
            Self::InnerDiameter => "InnerDiameter",
            Self::OuterDiameter => "OuterDiameter",
            Self::GrossHeight => "GrossHeight",
            Self::NetHeight => "NetHeight",
            Self::EavesHeight => "EavesHeight",
            Self::FinishCeilingHeight => "FinishCeilingHeight",
            Self::FinishFloorHeight => "FinishFloorHeight",
            Self::PlanLength => "PlanLength",
            Self::Volume => "Volume",
            Self::GrossCrossSectionArea => "GrossCrossSectionArea",
            Self::NetCrossSectionArea => "NetCrossSectionArea",
            Self::GrossCeilingArea => "GrossCeilingArea",
            Self::NetCeilingArea => "NetCeilingArea",
            Self::GrossWallArea => "GrossWallArea",
            Self::NetWallArea => "NetWallArea",
            Self::FootprintArea => "FootprintArea",
            Self::TotalSurfaceArea => "TotalSurfaceArea",
            Self::ProjectedArea => "ProjectedArea",
            Self::PlanArea => "PlanArea",
            Self::SignArea => "SignArea",
        }
    }

    /// Parse a standard IFC quantity name back into a kind.
    ///
    /// Case-insensitive: bSDD IFC4x3 writes `GrossFootPrintArea` while IFC4 and
    /// every model in the validation corpus write `GrossFootprintArea`, and a
    /// mismatch would duplicate the quantity rather than fill it.
    pub fn from_ifc_name(name: &str) -> Option<Self> {
        const ALL: &[QuantityKind] = &[
            QuantityKind::Length, QuantityKind::Height, QuantityKind::Width,
            QuantityKind::Depth, QuantityKind::GrossVolume, QuantityKind::NetVolume,
            QuantityKind::GrossArea, QuantityKind::NetArea, QuantityKind::Area,
            QuantityKind::GrossFootprintArea, QuantityKind::NetFootprintArea,
            QuantityKind::GrossSideArea, QuantityKind::NetSideArea,
            QuantityKind::GrossFloorArea, QuantityKind::NetFloorArea,
            QuantityKind::CrossSectionArea, QuantityKind::OuterSurfaceArea,
            QuantityKind::GrossPerimeter, QuantityKind::NetPerimeter,
            QuantityKind::Perimeter, QuantityKind::Thickness, QuantityKind::Diameter,
            QuantityKind::InnerDiameter, QuantityKind::OuterDiameter,
            QuantityKind::GrossHeight, QuantityKind::NetHeight,
            QuantityKind::EavesHeight, QuantityKind::FinishCeilingHeight,
            QuantityKind::FinishFloorHeight, QuantityKind::PlanLength,
            QuantityKind::Volume, QuantityKind::GrossCrossSectionArea,
            QuantityKind::NetCrossSectionArea, QuantityKind::GrossCeilingArea,
            QuantityKind::NetCeilingArea, QuantityKind::GrossWallArea,
            QuantityKind::NetWallArea, QuantityKind::FootprintArea,
            QuantityKind::TotalSurfaceArea, QuantityKind::ProjectedArea,
            QuantityKind::PlanArea, QuantityKind::SignArea,
            QuantityKind::GrossSurfaceArea, QuantityKind::NetSurfaceArea,
        ];
        ALL.iter()
            .copied()
            .find(|k| k.ifc_name().eq_ignore_ascii_case(name.trim()))
    }
}

/// The quantity set an IFC class should carry, and the quantities in it.
pub struct QtoSpec {
    pub set_name: &'static str,
    pub quantities: Vec<QuantityKind>,
}

/// Generated from the vendored bSDD IFC4x3 index by
/// `scripts/build_qto_index.py`: 508 rows over 498 IFC classes.
///
/// This replaced a hand-written table of 14 types whose fallback asked for a
/// single `GrossVolume`, which is why proxies, railings, coverings and ~480
/// other classes never received the quantities they are defined to have.
const QTO_SETS: &str = include_str!("../resources/qto_sets.tsv");

type SpecTable = HashMap<&'static str, (&'static str, Vec<QuantityKind>)>;

fn spec_table() -> &'static SpecTable {
    static TABLE: OnceLock<SpecTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map: SpecTable = HashMap::new();
        for line in QTO_SETS.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(class), Some(set_name), Some(quantities)) =
                (cols.next(), cols.next(), cols.next())
            else {
                continue;
            };
            let kinds: Vec<QuantityKind> = quantities
                .split(',')
                .filter_map(QuantityKind::from_ifc_name)
                .collect();
            if kinds.is_empty() {
                continue;
            }
            // A handful of classes carry more than one quantity set. Merge them:
            // the audit only needs to know which quantities are expected, and
            // the set name is used solely to name a newly created container.
            map.entry(class)
                .and_modify(|(_, existing)| {
                    for k in &kinds {
                        if !existing.contains(k) {
                            existing.push(*k);
                        }
                    }
                })
                .or_insert((set_name, kinds));
        }
        map
    })
}

/// Return the quantity-set spec for an IFC entity name (any casing).
///
/// `None` means bSDD defines no geometrically-derivable quantity set for this
/// class — MEP devices such as actuators and alarms define only weight and
/// count. Nothing should be computed or written for those.
pub fn qto_spec_for(entity_name: &str) -> Option<QtoSpec> {
    let key = entity_name.trim().to_ascii_lowercase();
    let table = spec_table();
    let found = table.get(key.as_str()).or_else(|| {
        // `IfcWallStandardCase` is a wall and carries `Qto_WallBaseQuantities`
        // like any other. The generated table comes from the bSDD IFC4x3 index,
        // where these subtypes no longer exist — they were folded back into
        // their base class — but IFC2X3 and IFC4 files are full of them:
        // essentially every wall in an ArchiCAD IFC2X3 export is an
        // `IfcWallStandardCase`.
        //
        // Missing them did not degrade those elements, it skipped them
        // outright: no report, no geometry, not one quantity. In one 155 MB
        // model that was 1,155 walls x 8 quantities = 9,240 values never
        // attempted.
        for suffix in ["standardcase", "elementedcase"] {
            if let Some(base) = key.strip_suffix(suffix) {
                if let Some(spec) = table.get(base) {
                    return Some(spec);
                }
            }
        }
        None
    })?;
    Some(QtoSpec {
        set_name: found.0,
        quantities: found.1.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_for(class: &str) -> Vec<&'static str> {
        let mut v: Vec<_> = qto_spec_for(class)
            .expect("class should have a spec")
            .quantities
            .iter()
            .map(|k| k.ifc_name())
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn table_covers_the_expected_breadth() {
        assert!(
            spec_table().len() >= 450,
            "table looks truncated: {} classes",
            spec_table().len()
        );
    }

    /// The complaint that started this: proxies used to get a single
    /// GrossVolume from the catch-all fallback.
    #[test]
    fn building_element_proxy_gets_its_defined_quantities() {
        assert_eq!(
            kinds_for("IFCBUILDINGELEMENTPROXY"),
            vec!["NetSurfaceArea", "NetVolume"]
        );
    }

    #[test]
    fn wall_matches_the_ifc4_base_quantities() {
        assert_eq!(
            kinds_for("IFCWALL"),
            vec![
                "GrossFootprintArea", "GrossSideArea", "GrossVolume", "Height",
                "Length", "NetFootprintArea", "NetSideArea", "NetVolume", "Width",
            ]
        );
    }

    /// bSDD spells it GrossFootPrintArea; IFC4 and every model in the validation
    /// corpus use the lowercase-p form. Emitting bSDD's spelling would duplicate
    /// the quantity instead of filling it.
    #[test]
    fn footprint_area_uses_the_ifc4_spelling() {
        let names = kinds_for("IFCWALL");
        assert!(names.contains(&"GrossFootprintArea"));
        assert!(!names.contains(&"GrossFootPrintArea"));
    }

    #[test]
    fn space_gets_floor_area_and_height() {
        let names = kinds_for("IFCSPACE");
        for expected in ["GrossFloorArea", "NetFloorArea", "Height", "GrossPerimeter"] {
            assert!(names.contains(&expected), "IfcSpace should define {expected}");
        }
    }

    #[test]
    fn entity_name_casing_does_not_matter() {
        assert!(qto_spec_for("IfcWall").is_some());
        assert!(qto_spec_for("IFCWALL").is_some());
        assert!(qto_spec_for("ifcwall").is_some());
    }

    /// Weight and count are not derivable from geometry, so classes defining only
    /// those get no spec at all rather than one that can never be satisfied.
    #[test]
    fn classes_with_only_non_geometric_quantities_have_no_spec() {
        assert!(qto_spec_for("IFCACTUATOR").is_none());
        assert!(qto_spec_for("IFCALARM").is_none());
    }

    #[test]
    fn unknown_classes_have_no_spec() {
        assert!(qto_spec_for("IFCNOTATHING").is_none());
    }

    /// IFC2X3 and IFC4 are full of the deprecated `*StandardCase` /
    /// `*ElementedCase` subtypes — essentially every wall in an ArchiCAD IFC2X3
    /// export is an `IfcWallStandardCase` — and IFC4x3, which the generated
    /// table comes from, folded them all back into their base class. Missing
    /// them skipped those elements outright rather than degrading them.
    #[test]
    fn deprecated_case_subtypes_resolve_to_their_base_class() {
        for (subtype, base) in [
            ("IFCWALLSTANDARDCASE", "IFCWALL"),
            ("IFCWALLELEMENTEDCASE", "IFCWALL"),
            ("IFCSLABSTANDARDCASE", "IFCSLAB"),
            ("IFCSLABELEMENTEDCASE", "IFCSLAB"),
            ("IFCDOORSTANDARDCASE", "IFCDOOR"),
            ("IFCWINDOWSTANDARDCASE", "IFCWINDOW"),
            ("IFCBEAMSTANDARDCASE", "IFCBEAM"),
            ("IFCCOLUMNSTANDARDCASE", "IFCCOLUMN"),
            ("IFCMEMBERSTANDARDCASE", "IFCMEMBER"),
            ("IFCPLATESTANDARDCASE", "IFCPLATE"),
        ] {
            let sub = qto_spec_for(subtype)
                .unwrap_or_else(|| panic!("{subtype} must resolve"));
            let b = qto_spec_for(base).expect("base class");
            assert_eq!(sub.set_name, b.set_name, "{subtype}");
            assert_eq!(sub.quantities, b.quantities, "{subtype}");
        }
    }

    /// The fallback must not invent a spec for a class that genuinely has none.
    #[test]
    fn the_fallback_does_not_manufacture_specs() {
        assert!(qto_spec_for("IFCNOTATHINGSTANDARDCASE").is_none());
    }
}
