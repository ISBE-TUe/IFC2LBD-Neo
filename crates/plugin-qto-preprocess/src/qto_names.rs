/// Maps IFC entity type names to their IFC4 standard quantity set name and
/// the ordered list of quantity kinds expected in that set.

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
    GrossPerimeter,
    Perimeter,
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
            | Self::Perimeter => "IFCQUANTITYLENGTH",

            Self::GrossVolume | Self::NetVolume => "IFCQUANTITYVOLUME",

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
            Self::GrossPerimeter => "GrossPerimeter",
            Self::Perimeter => "Perimeter",
        }
    }
}

pub struct QtoSpec {
    pub set_name: &'static str,
    pub quantities: &'static [QuantityKind],
}

/// Return the standard Qto_* spec for the given uppercase IFC entity name,
/// or a generic fallback if the type is not in the table.
pub fn qto_spec_for(entity_name: &str) -> QtoSpec {
    match entity_name {
        "IFCWALL" | "IFCWALLSTANDARDCASE" => QtoSpec {
            set_name: "Qto_WallBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::Width,
                QuantityKind::Height,
                QuantityKind::GrossFootprintArea,
                QuantityKind::NetFootprintArea,
                QuantityKind::GrossSideArea,
                QuantityKind::NetSideArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCSLAB" => QtoSpec {
            set_name: "Qto_SlabBaseQuantities",
            quantities: &[
                QuantityKind::Depth,
                QuantityKind::Perimeter,
                QuantityKind::GrossArea,
                QuantityKind::NetArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCBEAM" => QtoSpec {
            set_name: "Qto_BeamBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::CrossSectionArea,
                QuantityKind::OuterSurfaceArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCCOLUMN" => QtoSpec {
            set_name: "Qto_ColumnBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::CrossSectionArea,
                QuantityKind::OuterSurfaceArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCDOOR" => QtoSpec {
            set_name: "Qto_DoorBaseQuantities",
            quantities: &[
                QuantityKind::Width,
                QuantityKind::Height,
                QuantityKind::Perimeter,
                QuantityKind::Area,
            ],
        },
        "IFCWINDOW" => QtoSpec {
            set_name: "Qto_WindowBaseQuantities",
            quantities: &[
                QuantityKind::Width,
                QuantityKind::Height,
                QuantityKind::Perimeter,
                QuantityKind::Area,
            ],
        },
        "IFCSPACE" => QtoSpec {
            set_name: "Qto_SpaceBaseQuantities",
            quantities: &[
                QuantityKind::Height,
                QuantityKind::GrossPerimeter,
                QuantityKind::GrossFloorArea,
                QuantityKind::NetFloorArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCSTAIR" => QtoSpec {
            set_name: "Qto_StairBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCROOF" => QtoSpec {
            set_name: "Qto_RoofBaseQuantities",
            quantities: &[QuantityKind::GrossArea, QuantityKind::NetArea],
        },
        "IFCCOVERING" => QtoSpec {
            set_name: "Qto_CoveringBaseQuantities",
            quantities: &[
                QuantityKind::GrossArea,
                QuantityKind::NetArea,
            ],
        },
        "IFCFOOTING" => QtoSpec {
            set_name: "Qto_FootingBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCPILE" => QtoSpec {
            set_name: "Qto_PileBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCMEMBER" => QtoSpec {
            set_name: "Qto_MemberBaseQuantities",
            quantities: &[
                QuantityKind::Length,
                QuantityKind::CrossSectionArea,
                QuantityKind::OuterSurfaceArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        "IFCPLATE" => QtoSpec {
            set_name: "Qto_PlateBaseQuantities",
            quantities: &[
                QuantityKind::GrossArea,
                QuantityKind::NetArea,
                QuantityKind::GrossVolume,
                QuantityKind::NetVolume,
            ],
        },
        _ => QtoSpec {
            set_name: "Qto_ElementBaseQuantities",
            quantities: &[QuantityKind::GrossVolume],
        },
    }
}
