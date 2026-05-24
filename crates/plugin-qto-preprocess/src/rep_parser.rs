/// Tier 2: exact quantities from IFCEXTRUDEDAREASOLID + profile definitions.
///
/// When a shape is an extruded solid, we can compute exact cross-section area,
/// perimeter, and volume from the profile geometry without tessellation.

use std::f64::consts::PI;

use ifc_step::{EntityId, StepFile};

use crate::step_geom::{collect_points, real_from_step};

#[derive(Debug, Clone)]
pub struct RepResult {
    /// Extrusion depth (becomes Length for beams/columns, Height for walls).
    pub extrusion_depth: f64,
    /// Cross-section profile area.
    pub profile_area: f64,
    /// Cross-section profile perimeter.
    pub profile_perimeter: f64,
}

impl RepResult {
    pub fn net_volume(&self) -> f64 {
        self.extrusion_depth * self.profile_area
    }

    pub fn outer_surface_area(&self) -> f64 {
        self.extrusion_depth * self.profile_perimeter
    }
}

/// Try to parse an IFCEXTRUDEDAREASOLID entity into a `RepResult`.
pub fn from_extruded_solid(step: &StepFile, profile_id: EntityId, depth: f64) -> Option<RepResult> {
    let (area, perimeter) = profile_area_perimeter(step, profile_id)?;
    Some(RepResult {
        extrusion_depth: depth,
        profile_area: area,
        profile_perimeter: perimeter,
    })
}

/// Compute (area, perimeter) for a profile definition entity.
fn profile_area_perimeter(step: &StepFile, profile_id: EntityId) -> Option<(f64, f64)> {
    let e = step.entities.get(&profile_id)?;
    match e.entity_name.as_str() {
        "IFCRECTANGLEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, XDim, YDim)
            let x = real_from_step(e.args.get(3)?)?;
            let y = real_from_step(e.args.get(4)?)?;
            Some((x * y, 2.0 * (x + y)))
        }
        "IFCRECTANGLEHOLLOWPROFILEDEF" => {
            // outer XDim/YDim at args[3..4], wall thickness at args[5]
            let x = real_from_step(e.args.get(3)?)?;
            let y = real_from_step(e.args.get(4)?)?;
            let t = real_from_step(e.args.get(5)?).unwrap_or(0.0);
            let outer_area = x * y;
            let inner_x = (x - 2.0 * t).max(0.0);
            let inner_y = (y - 2.0 * t).max(0.0);
            Some((outer_area - inner_x * inner_y, 2.0 * (x + y)))
        }
        "IFCCIRCLEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, Radius)
            let r = real_from_step(e.args.get(3)?)?;
            Some((PI * r * r, 2.0 * PI * r))
        }
        "IFCCIRCLEHOLLOWPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, Radius, WallThickness)
            let r = real_from_step(e.args.get(3)?)?;
            let t = real_from_step(e.args.get(4)?).unwrap_or(0.0);
            let r_inner = (r - t).max(0.0);
            Some((PI * (r * r - r_inner * r_inner), 2.0 * PI * r))
        }
        "IFCELLIPSEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, SemiAxis1, SemiAxis2)
            let a = real_from_step(e.args.get(3)?)?;
            let b = real_from_step(e.args.get(4)?)?;
            // Ramanujan approximation for perimeter
            let perimeter = PI * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt());
            Some((PI * a * b, perimeter))
        }
        "IFCISHAPEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, OverallWidth, OverallDepth,
            //        WebThickness, FlangeThickness, ...)
            let w = real_from_step(e.args.get(3)?)?;
            let d = real_from_step(e.args.get(4)?)?;
            let tw = real_from_step(e.args.get(5)?)?;
            let tf = real_from_step(e.args.get(6)?)?;
            let area = 2.0 * w * tf + (d - 2.0 * tf) * tw;
            let perimeter = 2.0 * (w + d); // approx outer perimeter
            Some((area, perimeter))
        }
        "IFCTSHAPEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, Depth, FlangeWidth,
            //        WebThickness, FlangeThickness, ...)
            let d = real_from_step(e.args.get(3)?)?;
            let fw = real_from_step(e.args.get(4)?)?;
            let tw = real_from_step(e.args.get(5)?)?;
            let tf = real_from_step(e.args.get(6)?)?;
            let area = fw * tf + (d - tf) * tw;
            let perimeter = fw + 2.0 * d + tw; // rough approx
            Some((area, perimeter))
        }
        "IFCLSHAPEPROFILEDEF" => {
            // args: (ProfileType, ProfileName, Position, Depth, Width, Thickness, ...)
            let depth = real_from_step(e.args.get(3)?)?;
            let width = real_from_step(e.args.get(4)?)?;
            let t = real_from_step(e.args.get(5)?)?;
            let area = depth * t + (width - t) * t;
            let perimeter = depth + width + depth - t + width - t;
            Some((area, perimeter))
        }
        "IFCARBITRARYCLOSEDPROFILEDEF" => {
            // args: (ProfileType, ProfileName, OuterCurve)
            let curve_id = e.args.get(2)?.as_ref()?;
            arbitrary_closed_profile(step, curve_id)
        }
        "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            // args: (ProfileType, ProfileName, OuterCurve, InnerCurves)
            let outer_id = e.args.get(2)?.as_ref()?;
            let (outer_area, outer_perimeter) = arbitrary_closed_profile(step, outer_id)?;
            // Subtract inner voids
            let mut net_area = outer_area;
            if let Some(inners) = e.args.get(3).and_then(StepValue::as_list) {
                for inner_val in inners {
                    if let Some(inner_id) = inner_val.as_ref() {
                        if let Some((inner_area, _)) = arbitrary_closed_profile(step, inner_id) {
                            net_area -= inner_area;
                        }
                    }
                }
            }
            Some((net_area.max(0.0), outer_perimeter))
        }
        _ => None,
    }
}

/// Compute (area, perimeter) for a closed curve via shoelace formula.
fn arbitrary_closed_profile(
    step: &StepFile,
    curve_id: EntityId,
) -> Option<(f64, f64)> {
    let curve = step.entities.get(&curve_id)?;
    let points = match curve.entity_name.as_str() {
        "IFCPOLYLINE" => {
            // args[0] = list of IFCCARTESIANPOINT refs
            let pts_list = curve.args.first()?.as_list()?;
            pts_list
                .iter()
                .filter_map(|v| v.as_ref())
                .filter_map(|id| {
                    step.entities
                        .get(&id)
                        .and_then(|e| crate::step_geom::parse_cartesian_point(e))
                })
                .collect::<Vec<_>>()
        }
        "IFCINDEXEDPOLYCURVE" => {
            // IFC4: args[0] = ref to IFCCARTESIANPOINTLIST2D/3D,
            //       args[1] = list of IFCLINEINDEX segments (optional).
            //
            // Multiple curves can share the same point list using different
            // IFCLINEINDEX sub-ranges, so we MUST use the segment indices and
            // NOT simply collect all points from the list.
            let pts_list_id = curve.args.first()?.as_ref()?;
            let all_pts = collect_points(step, pts_list_id, 2);
            if all_pts.is_empty() {
                return None;
            }

            // Try to extract ordered polygon from IFCLINEINDEX segments.
            if let Some(segments) = curve.args.get(1).and_then(StepValue::as_list) {
                let mut ordered: Vec<[f64; 3]> = Vec::new();
                for seg in segments {
                    // Each segment is IFCLINEINDEX((i1,i2,...,iN)) — 1-based indices.
                    let indices = match seg {
                        StepValue::Typed { value, .. } => value.as_list(),
                        StepValue::List(_) => seg.as_list(),
                        _ => None,
                    };
                    if let Some(idx_list) = indices {
                        for idx_val in idx_list {
                            if let Some(idx) = idx_val.as_int() {
                                let i = (idx as usize).saturating_sub(1);
                                if let Some(&pt) = all_pts.get(i) {
                                    ordered.push(pt);
                                }
                            }
                        }
                    }
                }
                // Deduplicate consecutive duplicate closing point (last == first).
                if ordered.len() > 1 && ordered.first() == ordered.last() {
                    ordered.pop();
                }
                if !ordered.is_empty() {
                    return if ordered.len() < 3 {
                        None
                    } else {
                        let (area, perimeter) = shoelace_2d(&ordered);
                        Some((area.abs(), perimeter))
                    };
                }
            }

            // No segments (or empty): fall back to all points in list order.
            all_pts
        }
        "IFCCOMPOSITECURVE" => {
            // Collect all points under the composite curve recursively.
            collect_points(step, curve_id, 8)
        }
        _ => return None,
    };

    if points.len() < 3 {
        return None;
    }

    // Shoelace in XY plane (profile def is always in local 2D / XY).
    let (area, perimeter) = shoelace_2d(&points);
    Some((area.abs(), perimeter))
}

fn shoelace_2d(pts: &[[f64; 3]]) -> (f64, f64) {
    let n = pts.len();
    let mut area = 0.0;
    let mut perimeter = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        let (xi, yi) = (pts[i][0], pts[i][1]);
        let (xj, yj) = (pts[j][0], pts[j][1]);
        area += xi * yj - xj * yi;
        let dx = xj - xi;
        let dy = yj - yi;
        perimeter += (dx * dx + dy * dy).sqrt();
    }
    (area / 2.0, perimeter)
}

// Bring StepValue into scope for the arbitrary profile void parser.
use ifc_step::StepValue;

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_step::parse_step_bytes;

    /// IFC4 wall: IFCEXTRUDEDAREASOLID + IFCARBITRARYCLOSEDPROFILEDEF +
    /// IFCINDEXEDPOLYCURVE + IFCCARTESIANPOINTLIST2D.
    /// Profile is 5.25m × 0.24m rectangle → area = 1.26m², depth = 2.75m → NetVolume = 3.465m³
    const IFC4_WALL_INDEXED: &[u8] = b"\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('','',(''),(''),'',' ','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#30=IFCEXTRUDEDAREASOLID(#31,#21,#23,2.75);\n\
#31=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,'',#47);\n\
#47=IFCINDEXEDPOLYCURVE(#49,(IFCLINEINDEX((1,2,3,4,1))),$);\n\
#49=IFCCARTESIANPOINTLIST2D(((0.,-0.24),(5.25,-0.24),(5.25,0.),(0.,0.)));\n\
#21=IFCAXIS2PLACEMENT3D(#22,$,$);\n\
#22=IFCCARTESIANPOINT((0.,0.,0.));\n\
#23=IFCDIRECTION((0.,0.,1.));\n\
ENDSEC;\n\
END-ISO-10303-21;\n\
";

    #[test]
    fn ifc4_indexed_polycurve_profile_area() {
        let step = parse_step_bytes(IFC4_WALL_INDEXED).expect("parse step");
        let profile_id = 31u64;
        let depth = 2.75f64;
        let result = from_extruded_solid(&step, profile_id, depth)
            .expect("should parse IFC4 indexed polycurve profile");

        let expected_area = 5.25 * 0.24;
        let expected_volume = expected_area * depth;

        assert!(
            (result.profile_area - expected_area).abs() < 1e-6,
            "profile_area = {}, expected ~{}", result.profile_area, expected_area
        );
        assert!(
            (result.net_volume() - expected_volume).abs() < 1e-6,
            "net_volume = {}, expected ~{}", result.net_volume(), expected_volume
        );
    }

    #[test]
    fn ifc4_collect_points_from_indexed_polycurve() {
        use crate::step_geom::collect_points;
        let step = parse_step_bytes(IFC4_WALL_INDEXED).expect("parse step");

        // Start from the IFCINDEXEDPOLYCURVE entity (#47), collect all points.
        let pts = collect_points(&step, 47u64, 6);
        assert!(
            pts.len() >= 4,
            "should collect 4 profile polygon points, got {:?}", pts
        );

        // The point list has x in [0, 5.25] and y in [-0.24, 0].
        let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_x - 5.25).abs() < 1e-6,
            "max x should be ~5.25, got {}", max_x
        );
    }
}
