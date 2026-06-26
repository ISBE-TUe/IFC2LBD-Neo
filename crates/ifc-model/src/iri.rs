//! Canonical resource-IRI construction for IFC elements and spatial nodes.
//!
//! Single source of truth shared by the LBD converter (which emits these IRIs as
//! RDF subjects) and the geometry producer (which stamps the same IRI onto each
//! 3D object so the viewer can link a clicked object straight to its LBD node,
//! without a reverse GUID lookup).
//!
//! Keep this in lockstep with the LBD output: any change here changes the RDF
//! subject IRIs too.

use ifc_schema::{product_type_name, SpatialType};

use crate::{compress_uuid_string, ElementNode, SpatialNode};

/// IFC GlobalIds are 22-char base64-compressed UUIDs; anything else is expanded
/// from a full UUID string into that canonical 22-char form when possible.
fn canonical_guid_token(raw: &str) -> String {
    if raw.len() == 22 {
        return raw.to_string();
    }
    compress_uuid_string(raw).unwrap_or_else(|| raw.to_string())
}

/// Make a GUID token safe to embed in an IRI local name: IFC's `$` and any other
/// non-`[A-Za-z0-9_]` character become `_`.
pub fn prefix_safe_guid_token(raw: &str) -> String {
    canonical_guid_token(raw)
        .chars()
        .map(|ch| match ch {
            '$' => '_',
            _ if ch.is_ascii_alphanumeric() || ch == '_' => ch,
            _ => '_',
        })
        .collect()
}

/// `<prefix>_<safe_guid>` local name.
pub fn lbd_local_name(prefix: &str, guid: &str) -> String {
    let suffix = prefix_safe_guid_token(guid);
    format!("{prefix}_{suffix}")
}

/// Convert an uppercase STEP entity name (`IFCWALLSTANDARDCASE`) to PascalCase
/// (`IfcWallStandardCase`).
pub fn pascal_ifc_name(entity_name: &str) -> String {
    let upper = entity_name.to_ascii_uppercase();
    if !upper.starts_with("IFC") {
        return entity_name.to_string();
    }
    let mut out = String::from("Ifc");
    let mut capitalize = true;
    for ch in upper[3..].chars() {
        if ch == '_' {
            capitalize = true;
        } else if ch.is_ascii_digit() {
            out.push(ch);
            capitalize = true;
        } else if capitalize {
            out.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn spatial_segment(spatial_type: SpatialType) -> &'static str {
    match spatial_type {
        SpatialType::Project => "project",
        SpatialType::Site => "site",
        SpatialType::Building => "building",
        SpatialType::Storey => "storey",
        SpatialType::Space => "space",
        SpatialType::Zone
        | SpatialType::Facility
        | SpatialType::FacilityPart
        | SpatialType::ExternalSpatialElement => "zone",
    }
}

/// Resource IRI for a spatial node, e.g. `<base>/storey_<guid>`.
pub fn spatial_resource_iri(base: &str, spatial_type: SpatialType, guid: &str) -> String {
    format!(
        "{base}/{}",
        lbd_local_name(spatial_segment(spatial_type), guid)
    )
}

/// Resource IRI for an element node, e.g. `<base>/wall_<guid>`.
///
/// The prefix mirrors the Java LBD converter: building element proxies use the
/// generic `buildingelement` prefix, recognised product types use their lowercase
/// product name, and everything else falls back to `ifcowl_<lowercasepascal>`.
pub fn element_resource_iri(base: &str, element: &ElementNode) -> String {
    format!(
        "{base}/{}",
        lbd_local_name(&element_prefix(element.entity_name.as_str()), &element.guid)
    )
}

fn element_prefix(entity_name: &str) -> String {
    match entity_name {
        "IFCBUILDINGELEMENTPROXY" => "buildingelement".to_string(),
        _ => product_type_name(entity_name)
            .map(|name| name.to_ascii_lowercase())
            .unwrap_or_else(|| {
                format!(
                    "ifcowl_{}",
                    pascal_ifc_name(entity_name).to_ascii_lowercase()
                )
            }),
    }
}

/// Resource IRI for a spatial node value object.
pub fn spatial_node_resource_iri(base: &str, node: &SpatialNode) -> String {
    spatial_resource_iri(base, node.spatial_type, node.guid.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_safe_guid_token_rewrites_ifc_special_chars() {
        assert_eq!(
            prefix_safe_guid_token("2O2Fr$t4X7Zf8NOew3FNtn"),
            "2O2Fr_t4X7Zf8NOew3FNtn"
        );
    }

    #[test]
    fn canonical_guid_token_compresses_expanded_uuid() {
        let expanded = "7b7032cc-b822-417b-9aea-642906a29bd5";
        assert_eq!(canonical_guid_token(expanded), "1xS3BCk291UvhgP2a6eflL");
    }
}
