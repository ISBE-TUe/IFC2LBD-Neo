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
