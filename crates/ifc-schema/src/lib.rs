//! Minimal IFC schema lookup helpers for the first model-building slice.

/// IFC spatial structure types needed for the initial BOT hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpatialType {
    Project,
    Site,
    Building,
    Storey,
    Space,
}

impl SpatialType {
    pub fn as_ifc_name(self) -> &'static str {
        match self {
            SpatialType::Project => "IFCPROJECT",
            SpatialType::Site => "IFCSITE",
            SpatialType::Building => "IFCBUILDING",
            SpatialType::Storey => "IFCBUILDINGSTOREY",
            SpatialType::Space => "IFCSPACE",
        }
    }
}

pub fn spatial_type(entity_name: &str) -> Option<SpatialType> {
    match entity_name {
        "IFCPROJECT" => Some(SpatialType::Project),
        "IFCSITE" => Some(SpatialType::Site),
        "IFCBUILDING" => Some(SpatialType::Building),
        "IFCBUILDINGSTOREY" => Some(SpatialType::Storey),
        "IFCSPACE" => Some(SpatialType::Space),
        _ => None,
    }
}

pub fn is_spatial_structure(entity_name: &str) -> bool {
    spatial_type(entity_name).is_some()
}

pub fn is_element(entity_name: &str) -> bool {
    matches!(
        entity_name,
        "IFCBEAM"
            | "IFCBUILDINGELEMENTPROXY"
            | "IFCCOLUMN"
            | "IFCCOVERING"
            | "IFCCURTAINWALL"
            | "IFCDOOR"
            | "IFCELEMENTASSEMBLY"
            | "IFCFOOTING"
            | "IFCFURNISHINGELEMENT"
            | "IFCMEMBER"
            | "IFCOPENINGELEMENT"
            | "IFCPLATE"
            | "IFCRAILING"
            | "IFCROOF"
            | "IFCSLAB"
            | "IFCSTAIR"
            | "IFCSTAIRFLIGHT"
            | "IFCWALL"
            | "IFCWALLSTANDARDCASE"
            | "IFCWINDOW"
    )
}

pub fn is_relationship(entity_name: &str) -> bool {
    matches!(
        entity_name,
        "IFCRELAGGREGATES" | "IFCRELCONTAINEDINSPATIALSTRUCTURE"
    )
}

pub fn product_type_name(entity_name: &str) -> Option<&'static str> {
    match entity_name {
        "IFCWALL" | "IFCWALLSTANDARDCASE" => Some("Wall"),
        "IFCDOOR" => Some("Door"),
        "IFCWINDOW" => Some("Window"),
        "IFCSLAB" => Some("Slab"),
        "IFCROOF" => Some("Roof"),
        "IFCBEAM" => Some("Beam"),
        "IFCRAILING" => Some("Railing"),
        "IFCSTAIR" => Some("Stair"),
        "IFCSTAIRFLIGHT" => Some("StairFlight"),
        "IFCCOLUMN" => Some("Column"),
        "IFCCOVERING" => Some("Covering"),
        "IFCPLATE" => Some("Plate"),
        "IFCMEMBER" => Some("Member"),
        "IFCFOOTING" => Some("Footing"),
        "IFCCURTAINWALL" => Some("CurtainWall"),
        "IFCBUILDINGELEMENTPROXY" => Some("BuildingElement"),
        "IFCFURNISHINGELEMENT" => Some("Furniture"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spatial_lookup() {
        assert_eq!(spatial_type("IFCBUILDINGSTOREY"), Some(SpatialType::Storey));
        assert!(is_spatial_structure("IFCSPACE"));
        assert!(!is_spatial_structure("IFCWALL"));
    }

    #[test]
    fn test_element_lookup() {
        assert!(is_element("IFCWALLSTANDARDCASE"));
        assert!(is_element("IFCDOOR"));
        assert!(is_element("IFCELEMENTASSEMBLY"));
        assert!(is_element("IFCOPENINGELEMENT"));
        assert!(!is_element("IFCRELAGGREGATES"));
    }

    #[test]
    fn test_product_type_name() {
        assert_eq!(product_type_name("IFCWALLSTANDARDCASE"), Some("Wall"));
        assert_eq!(product_type_name("IFCWINDOW"), Some("Window"));
        assert_eq!(product_type_name("IFCSTAIRFLIGHT"), Some("StairFlight"));
        assert_eq!(product_type_name("IFCRELAGGREGATES"), None);
    }
}
