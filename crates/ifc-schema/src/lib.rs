//! Minimal IFC schema lookup helpers for the first model-building slice.

/// IFC spatial structure types needed for the initial BOT hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpatialType {
    Project,
    Site,
    Building,
    Storey,
    Space,
    /// IFC4: IfcSpatialZone
    Zone,
    /// IFC4.3: IfcFacility, IfcBridge, IfcRoad, IfcRailway, IfcMarineFacility
    Facility,
    /// IFC4.3: IfcFacilityPart
    FacilityPart,
    /// IFC4: IfcExternalSpatialElement
    ExternalSpatialElement,
}

impl SpatialType {
    pub fn as_ifc_name(self) -> &'static str {
        match self {
            SpatialType::Project => "IFCPROJECT",
            SpatialType::Site => "IFCSITE",
            SpatialType::Building => "IFCBUILDING",
            SpatialType::Storey => "IFCBUILDINGSTOREY",
            SpatialType::Space => "IFCSPACE",
            SpatialType::Zone => "IFCSPATIALZONE",
            SpatialType::Facility => "IFCFACILITY",
            SpatialType::FacilityPart => "IFCFACILITYPART",
            SpatialType::ExternalSpatialElement => "IFCEXTERNALSPATIALELEMENT",
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
        "IFCSPATIALZONE" => Some(SpatialType::Zone),
        "IFCFACILITY" | "IFCBRIDGE" | "IFCROAD" | "IFCRAILWAY" | "IFCMARINEFACILITY" => {
            Some(SpatialType::Facility)
        }
        "IFCFACILITYPART" | "IFCFACILITYPARTCOMMON" | "IFCBRIDGEPART" | "IFCROADPART"
        | "IFCRAILWAYPART" | "IFCMARINEFACILITYPART" => Some(SpatialType::FacilityPart),
        "IFCEXTERNALSPATIALELEMENT" => Some(SpatialType::ExternalSpatialElement),
        _ => None,
    }
}

pub fn is_spatial_structure(entity_name: &str) -> bool {
    spatial_type(entity_name).is_some()
}

pub fn is_element(entity_name: &str) -> bool {
    matches!(
        entity_name,
        // ── Core building elements (IFC2X3+) ─────────────────────────────────
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
            // ── IFC2X3 types missing from original list ───────────────────────
            | "IFCRAMP"
            | "IFCRAMPFLIGHT"
            | "IFCPILE"
            | "IFCVIRTUALELEMENT"
            | "IFCBUILDINGELEMENTPART"
            | "IFCFASTENER"
            | "IFCMECHANICALFASTENER"
            | "IFCDISCRETEACCESSORY"
            | "IFCREINFORCINGBAR"
            | "IFCREINFORCINGMESH"
            | "IFCTENDON"
            | "IFCTENDONANCHOR"
            | "IFCTRANSPORTELEMENT"
            // ── MEP abstract supertypes used directly in practice ─────────────
            | "IFCFLOWTERMINAL"
            | "IFCFLOWSEGMENT"
            | "IFCFLOWFITTING"
            | "IFCFLOWCONTROLLER"
            | "IFCFLOWMOVINGDEVICE"
            | "IFCFLOWSTORAGEDEVICE"
            | "IFCENERGYCONVERSIONDEVICE"
            | "IFCDISTRIBUTIONELEMENT"
            | "IFCDISTRIBUTIONFLOWDEVICE"
            | "IFCDISTRIBUTIONCONTROLDEVICE"
            | "IFCBUILDINGELEMENT"
            // ── MEP terminals (IFC2X3+) ───────────────────────────────────────
            | "IFCAIRTERMINAL"
            | "IFCFIRESUPPRESSIONTERMINAL"
            | "IFCSANITARYTERMINAL"
            | "IFCSPACEHEATER"
            | "IFCOUTLET"
            | "IFCSTACKTERMINAL"
            | "IFCWASTETERMINAL"
            // ── MEP distribution control (IFC2X3+) ───────────────────────────
            | "IFCDAMPER"
            | "IFCVALVE"
            | "IFCFLOWMETER"
            | "IFCPROTECTIVEDEVICE"
            // ── MEP flow moving (IFC2X3+) ─────────────────────────────────────
            | "IFCCOMPRESSOR"
            | "IFCFAN"
            | "IFCPUMP"
            // ── MEP fittings & segments (IFC2X3+) ────────────────────────────
            | "IFCDUCTFITTING"
            | "IFCDUCTSILENCER"
            | "IFCPIPEFITTING"
            | "IFCJUNCTIONBOX"
            | "IFCCABLECARRIERFITTING"
            | "IFCDUCTSEGMENT"
            | "IFCPIPESEGMENT"
            | "IFCCABLESEGMENT"
            | "IFCCABLECARRIERSEGMENT"
            // ── MEP storage & energy (IFC2X3+) ───────────────────────────────
            | "IFCTANK"
            | "IFCBOILER"
            | "IFCCHILLER"
            | "IFCCOIL"
            | "IFCCONDENSER"
            | "IFCCOOLINGTOWER"
            | "IFCELECTRICGENERATOR"
            | "IFCELECTRICMOTOR"
            | "IFCHEATEXCHANGER"
            | "IFCHUMIDIFIER"
            | "IFCUNITARYEQUIPMENT"
            // ── MEP sensors & controls (IFC2X3+) ─────────────────────────────
            | "IFCACTUATOR"
            | "IFCALARM"
            | "IFCCONTROLLER"
            | "IFCSENSOR"
            // ── New in IFC4 ───────────────────────────────────────────────────
            | "IFCCHIMNEY"
            | "IFCSHADINGDEVICE"
            | "IFCDEEPFOUNDATION"
            | "IFCAUDIOVISUALAPPLIANCE"
            | "IFCCOMMUNICATIONSAPPLIANCE"
            | "IFCELECTRICAPPLIANCE"
            | "IFCMEDICALDEVICE"
            | "IFCELECTRICDISTRIBUTIONBOARD"
            | "IFCSWITCHINGDEVICE"
            | "IFCLIGHTFIXTURE"
            | "IFCFURNITURE"
            | "IFCSYSTEMFURNITUREELEMENT"
            | "IFCELECTRICFLOWSTORAGEDEVICE"
            | "IFCFILTER"
            | "IFCINTERCEPTOR"
            | "IFCAIRTOAIRHEATRECOVERY"
            | "IFCBURNER"
            | "IFCCOOLEDBEAM"
            | "IFCEVAPORATIVECOOLER"
            | "IFCEVAPORATOR"
            | "IFCMOTORCONNECTION"
            | "IFCTUBESBUNDLE"
            | "IFCAIRTERMINALBOX"
            | "IFCCABLEFITTING"
            | "IFCFLOWINSTRUMENT"
            | "IFCPROTECTIVEDEVICETRIPPINGUNIT"
            | "IFCUNITARYCONTROLELEMENT"
            // ── New in IFC4.3 ─────────────────────────────────────────────────
            | "IFCBEARING"
            | "IFCCOURSE"
            | "IFCEARTHWORKSELEMENT"
            | "IFCKERB"
            | "IFCMOORINGDEVICE"
            | "IFCNAVIGATIONELEMENT"
            | "IFCPAVEMENT"
            | "IFCSIGN"
            | "IFCSIGNAL"
            | "IFCTRACKELELEMENT"
            | "IFCVOIDINGFEATURE"
            | "IFCELECTRICFLOWTREATMENTDEVICE"
            | "IFCTENDONCONDUICT"
            | "IFCSURFACEFEATURE"
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
        // IFC4 additions with BEO class coverage
        "IFCCHIMNEY" => Some("Chimney"),
        "IFCSHADINGDEVICE" => Some("ShadingDevice"),
        "IFCPILE" | "IFCDEEPFOUNDATION" => Some("Pile"),
        "IFCRAMP" => Some("Ramp"),
        "IFCRAMPFLIGHT" => Some("RampFlight"),
        "IFCFURNITURE" | "IFCSYSTEMFURNITUREELEMENT" => Some("Furniture"),
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
    fn test_spatial_lookup_ifc4() {
        assert_eq!(spatial_type("IFCSPATIALZONE"), Some(SpatialType::Zone));
        assert_eq!(spatial_type("IFCFACILITY"), Some(SpatialType::Facility));
        assert_eq!(spatial_type("IFCBRIDGE"), Some(SpatialType::Facility));
        assert_eq!(spatial_type("IFCROAD"), Some(SpatialType::Facility));
        assert_eq!(
            spatial_type("IFCFACILITYPART"),
            Some(SpatialType::FacilityPart)
        );
        assert_eq!(
            spatial_type("IFCEXTERNALSPATIALELEMENT"),
            Some(SpatialType::ExternalSpatialElement)
        );
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
    fn test_element_lookup_ifc4() {
        assert!(is_element("IFCLIGHTFIXTURE"));
        assert!(is_element("IFCTRANSPORTELEMENT"));
        assert!(is_element("IFCFURNITURE"));
        assert!(is_element("IFCCHIMNEY"));
        assert!(is_element("IFCBEARING"));
        assert!(is_element("IFCTRACKELELEMENT"));
        assert!(is_element("IFCSIGNAL"));
        assert!(is_element("IFCPAVEMENT"));
        assert!(!is_element("IFCPROJECT"));
    }

    #[test]
    fn test_product_type_name() {
        assert_eq!(product_type_name("IFCWALLSTANDARDCASE"), Some("Wall"));
        assert_eq!(product_type_name("IFCWINDOW"), Some("Window"));
        assert_eq!(product_type_name("IFCSTAIRFLIGHT"), Some("StairFlight"));
        assert_eq!(product_type_name("IFCRELAGGREGATES"), None);
        assert_eq!(product_type_name("IFCCHIMNEY"), Some("Chimney"));
        assert_eq!(product_type_name("IFCRAMP"), Some("Ramp"));
        assert_eq!(product_type_name("IFCPILE"), Some("Pile"));
    }
}
