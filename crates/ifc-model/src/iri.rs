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

/// Make a GUID token safe to embed in an IRI local name, **without losing
/// information**.
///
/// IFC GlobalIds are base64 over a 64-character alphabet that includes *both*
/// `_` and `$`. Rewriting `$` to `_` — which this did — is therefore not an
/// escape but a collision: `…TZzX$` and `…TZzX_` are two different objects and
/// both became `…TZzX_`. Everything hanging off them merged, so one RDF node
/// ended up carrying two walls' quantity sets, two geometries and two
/// containments. Measured on the corpus, that silently fused 527 objects in one
/// model, 56 in another and 24 in a third.
///
/// Anything outside `[A-Za-z0-9_]` is now percent-escaped instead. `%` cannot
/// occur in a GlobalId, so the mapping is injective, and standard
/// percent-decoding recovers the original identifier.
pub fn prefix_safe_guid_token(raw: &str) -> String {
    let mut out = String::new();
    for ch in canonical_guid_token(raw).chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            // UTF-8 bytes, so a non-ASCII character escapes to several octets —
            // the encoding a percent-escape is defined over.
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
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
    fn prefix_safe_guid_token_escapes_ifc_special_chars() {
        assert_eq!(
            prefix_safe_guid_token("2O2Fr$t4X7Zf8NOew3FNtn"),
            "2O2Fr%24t4X7Zf8NOew3FNtn"
        );
    }

    /// The defect this escaping exists for. `_` and `$` are both letters of the
    /// GlobalId alphabet, so two objects can differ only there; folding `$` onto
    /// `_` fused them into one RDF resource.
    #[test]
    fn dollar_and_underscore_do_not_collide() {
        let a = prefix_safe_guid_token("3LF03GdXv2GhSTK1xTZzX$");
        let b = prefix_safe_guid_token("3LF03GdXv2GhSTK1xTZzX_");
        assert_ne!(a, b, "distinct GlobalIds must not share a token");
        assert_eq!(a, "3LF03GdXv2GhSTK1xTZzX%24");
        assert_eq!(b, "3LF03GdXv2GhSTK1xTZzX_");
    }

    /// Every token the whole 64-letter GlobalId alphabet can produce must be
    /// distinct — the property that makes the IRI an identifier at all.
    #[test]
    fn the_whole_globalid_alphabet_maps_injectively() {
        const ALPHABET: &str =
            "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";
        let mut seen = std::collections::HashSet::new();
        for ch in ALPHABET.chars() {
            // 22 chars so `canonical_guid_token` passes it straight through.
            let guid: String = std::iter::once('0').chain(std::iter::repeat_n('0', 20)).chain(std::iter::once(ch)).collect();
            assert!(
                seen.insert(prefix_safe_guid_token(&guid)),
                "collision on {ch:?}"
            );
        }
        assert_eq!(seen.len(), 64);
    }

    #[test]
    fn canonical_guid_token_compresses_expanded_uuid() {
        let expanded = "7b7032cc-b822-417b-9aea-642906a29bd5";
        assert_eq!(canonical_guid_token(expanded), "1xS3BCk291UvhgP2a6eflL");
    }
}
