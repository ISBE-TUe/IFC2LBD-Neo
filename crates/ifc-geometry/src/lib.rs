//! Thin wrapper around ifc-lite-geometry and ifc-lite-core.
//!
//! Provides `stream_meshes` — the geometry extraction entry point for the
//! geometry pipeline. Takes raw IFC text and a list of element IDs, returns
//! tessellated meshes with world transforms and colors.
//!
//! ifc-lite is MPL-2.0. This wrapper is Apache-2.0 as part of the larger work.

pub use ifc_lite_geometry::mesh::Mesh;

/// A single geometry instance for one IFC element.
#[derive(Debug, Clone)]
pub struct FlatMesh {
    pub express_id: u64,
    pub guid: String,
    /// STEP entity name e.g. "IFCWALL"
    pub category: String,
    /// One entry per geometry item (body + openings already subtracted by ifc-lite).
    pub geometries: Vec<GeometryInstance>,
}

/// One geometry instance: tessellated mesh + color + transforms.
#[derive(Debug, Clone)]
pub struct GeometryInstance {
    /// GPU-ready: positions (flat f32 × 3), normals (flat f32 × 3), indices (u32)
    pub mesh: Mesh,
    /// RGBA 0-1 from IFCSTYLEDITEM chain
    pub color: [f32; 4],
    /// Column-major 4×4 world-space matrix
    pub world_transform: [f64; 16],
    /// Column-major 4×4 relative transform: `firstGeom^-1 × thisGeom` (identity for first)
    pub local_transform: [f64; 16],
    /// STEP express ID of the geometry entity — used as dedup key
    pub geometry_id: u64,
}

/// Process all elements in `element_ids` and return their tessellated meshes.
///
/// `ifc_content`: full raw IFC/STEP text (UTF-8).
/// `element_ids`: express IDs of elements to tessellate.
pub fn stream_meshes(ifc_content: &str, element_ids: &[u64]) -> Vec<FlatMesh> {
    use ifc_lite_core::{build_entity_index, EntityDecoder};
    use ifc_lite_geometry::router::GeometryRouter;

    if element_ids.is_empty() {
        return Vec::new();
    }

    let index = build_entity_index(ifc_content);
    let mut decoder = EntityDecoder::with_index(ifc_content, index);
    let router = GeometryRouter::with_units(ifc_content, &mut decoder);

    // Pre-build IFCSTYLEDITEM color map: geometry item express ID → RGBA
    let color_map = build_color_map(ifc_content, &mut decoder);

    let mut results = Vec::with_capacity(element_ids.len());

    for &eid in element_ids {
        let id = eid as u32;
        let element = match decoder.decode_by_id(id) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let guid = element.get(0).and_then(|a| a.as_string()).unwrap_or("").to_string();
        let category = element.ifc_type.to_string();

        // World transform
        let world_flat = match router.resolve_scaled_placement(&element, &mut decoder) {
            Ok(t) => t,
            Err(_) => IDENTITY_4X4,
        };

        // Oracle granularity: one sub-mesh per leaf geometry item (IFCEXTRUDEDAREASOLID etc.).
        // sub.local_matrix = accumulated(T×O) × itemPosition, geometry in item-local space (no Position baked).
        let sub_meshes = match router.process_element_submeshes_in_definition_space(&element, &mut decoder) {
            Ok(s) if !s.sub_meshes.is_empty() => s,
            _ => continue,
        };

        let n = sub_meshes.sub_meshes.len();
        let mut geometries = Vec::with_capacity(n);

        // Oracle: globalTransform = elementPlacement × T0 × O0
        //         localTransform[0] = identity
        //         localTransform[i] = (T0×O0)^-1 × (Ti×Oi)
        let elem_placement = nalgebra::Matrix4::from_column_slice(&world_flat);
        let first_to = sub_meshes.sub_meshes.first()
            .and_then(|s| s.local_matrix)
            .unwrap_or_else(nalgebra::Matrix4::identity);
        let first_to_inv = first_to.try_inverse()
            .unwrap_or_else(nalgebra::Matrix4::identity);
        // globalTransform = elementPlacement × T0 × O0 (matches oracle flatTransformation[0])
        let world_transform = mat4_to_col16(&(elem_placement * first_to));

        for (i, sub) in sub_meshes.sub_meshes.into_iter().enumerate() {
            if sub.mesh.positions.is_empty() { continue; }

            let color = color_map.get(&sub.geometry_id)
                .copied()
                .unwrap_or(DEFAULT_COLOR);

            // localTransform[0] = identity (oracle: first geometry is reference frame)
            // localTransform[i] = (T0×O0)^-1 × (Ti×Oi)
            let local_transform = if i == 0 {
                IDENTITY_4X4
            } else {
                let ti_oi = sub.local_matrix.unwrap_or_else(nalgebra::Matrix4::identity);
                mat4_to_col16(&(first_to_inv * ti_oi))
            };

            geometries.push(GeometryInstance {
                geometry_id: sub.geometry_id as u64,
                mesh: sub.mesh,
                color,
                world_transform,
                local_transform,
            });
        }

        if geometries.is_empty() { continue; }

        results.push(FlatMesh { express_id: eid, guid, category, geometries });
    }

    results
}

/// Build a map from geometry item express ID → RGBA color via IFCSTYLEDITEM chain.
///
/// IFCSTYLEDITEM(Item, Styles, Name)
///   → Styles → IFCSURFACESTYLE → IFCSURFACESTYLERENDERING
///     → SurfaceColour → IFCCOLOURRGB(Name, Red, Green, Blue)
///     + Transparency
fn build_color_map(
    content: &str,
    decoder: &mut ifc_lite_core::EntityDecoder,
) -> std::collections::HashMap<u32, [f32; 4]> {
    use ifc_lite_core::EntityScanner;
    use std::collections::HashMap;

    let mut map: HashMap<u32, [f32; 4]> = HashMap::new();

    let mut scanner = EntityScanner::new(content);
    while let Some((id, type_name, _start, _end)) = scanner.next_entity() {
        if type_name != "IFCSTYLEDITEM" { continue; }
        let Ok(entity) = decoder.decode_by_id(id) else { continue; };

        // arg[0] = Item (ref to geometry item)
        let Some(item_ref) = entity.get_ref(0) else { continue; };
        // arg[1] = Styles (list of style refs)
        let Some(styles_list) = entity.get_list(1) else { continue; };

        for style_attr in styles_list {
            let Some(style_id) = style_attr.as_entity_ref() else { continue; };
            if let Some(rgba) = resolve_style_color(decoder, style_id) {
                map.insert(item_ref, rgba);
                break;
            }
        }
    }

    map
}

fn resolve_style_color(
    decoder: &mut ifc_lite_core::EntityDecoder,
    style_id: u32,
) -> Option<[f32; 4]> {
    let entity = decoder.decode_by_id(style_id).ok()?;

    match entity.ifc_type.to_string().as_str() {
        "IfcPresentationStyleAssignment" | "IfcStyleAssignment" => {
            let styles = entity.get_list(0)?;
            for s in styles {
                let id = s.as_entity_ref()?;
                if let Some(c) = resolve_style_color(decoder, id) {
                    return Some(c);
                }
            }
            None
        }
        "IfcSurfaceStyle" => {
            // arg[2] = Styles list
            let styles = entity.get_list(2)?;
            for s in styles {
                let id = s.as_entity_ref()?;
                if let Some(c) = resolve_style_color(decoder, id) {
                    return Some(c);
                }
            }
            None
        }
        "IfcSurfaceStyleRendering" => {
            // arg[0] = SurfaceColour ref
            let colour_id = entity.get_ref(0)?;
            let colour = decoder.decode_by_id(colour_id).ok()?;
            if colour.ifc_type.to_string() != "IfcColourRgb" { return None; }
            let r = colour.get(1).and_then(|a| a.as_float()).unwrap_or(0.8) as f32;
            let g = colour.get(2).and_then(|a| a.as_float()).unwrap_or(0.8) as f32;
            let b = colour.get(3).and_then(|a| a.as_float()).unwrap_or(0.8) as f32;
            let transparency = entity.get(1).and_then(|a| a.as_float()).unwrap_or(0.0) as f32;
            let a = (1.0 - transparency.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            Some([r.clamp(0.0,1.0), g.clamp(0.0,1.0), b.clamp(0.0,1.0), a])
        }
        _ => None,
    }
}

const IDENTITY_4X4: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];

/// Convert nalgebra Matrix4 to column-major [f64; 16] array.
fn mat4_to_col16(m: &nalgebra::Matrix4<f64>) -> [f64; 16] {
    let s = m.as_slice();
    [s[0],s[1],s[2],s[3], s[4],s[5],s[6],s[7], s[8],s[9],s[10],s[11], s[12],s[13],s[14],s[15]]
}

const DEFAULT_COLOR: [f32; 4] = [0.8, 0.8, 0.8, 1.0];
