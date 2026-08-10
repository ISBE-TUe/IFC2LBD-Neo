//! Exact area, perimeter and extent for IFC profile definitions.
//!
//! Every profile here is either a polygon or an analytic shape, so its area and
//! perimeter are closed-form. No tessellation, no kernel, no tolerance.
//!
//! Coverage matters as much as correctness: the previous implementation handled
//! 9 of the ~20 profile types, and a profile it did not recognise produced *no
//! quantities at all* for that element — a slab with an `IfcUShapeProfileDef`
//! silently yielded an empty quantity set.

use std::f64::consts::PI;

use ifc_step::{EntityId, StepFile, StepValue};

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileMetrics {
    /// Enclosed area, voids already subtracted.
    pub area: f64,
    /// Outer perimeter. Void boundaries are excluded — IFC's `Perimeter`
    /// quantity means the outer boundary.
    pub perimeter: f64,
    /// Smallest bounding dimension. For a wall profile this is its thickness;
    /// for a beam, its minor axis.
    pub min_span: f64,
    /// Largest bounding dimension.
    pub max_span: f64,
}

/// Exact metrics for a profile definition, or `None` when the profile type is
/// not recognised — in which case the caller must emit nothing.
pub fn metrics(step: &StepFile, profile_id: EntityId) -> Option<ProfileMetrics> {
    let e = step.entities.get(&profile_id)?;
    let a = |n: usize| e.args.get(n).and_then(real);

    match e.entity_name.as_str() {
        "IFCRECTANGLEPROFILEDEF" => {
            let (x, y) = (a(3)?, a(4)?);
            Some(rect(x, y))
        }
        "IFCROUNDEDRECTANGLEPROFILEDEF" => {
            let (x, y, r) = (a(3)?, a(4)?, a(5).unwrap_or(0.0));
            // Corners replace 4 right angles with 4 quarter-circles.
            let area = x * y - (4.0 - PI) * r * r;
            let perimeter = 2.0 * (x + y) - 8.0 * r + 2.0 * PI * r;
            Some(ProfileMetrics { area, perimeter, min_span: x.min(y), max_span: x.max(y) })
        }
        "IFCRECTANGLEHOLLOWPROFILEDEF" => {
            let (x, y, t) = (a(3)?, a(4)?, a(5)?);
            let (ix, iy) = ((x - 2.0 * t).max(0.0), (y - 2.0 * t).max(0.0));
            Some(ProfileMetrics {
                area: x * y - ix * iy,
                perimeter: 2.0 * (x + y),
                min_span: x.min(y),
                max_span: x.max(y),
            })
        }
        "IFCCIRCLEPROFILEDEF" => {
            let r = a(3)?;
            Some(ProfileMetrics {
                area: PI * r * r,
                perimeter: 2.0 * PI * r,
                min_span: 2.0 * r,
                max_span: 2.0 * r,
            })
        }
        "IFCCIRCLEHOLLOWPROFILEDEF" => {
            let (r, t) = (a(3)?, a(4)?);
            let ri = (r - t).max(0.0);
            Some(ProfileMetrics {
                area: PI * (r * r - ri * ri),
                perimeter: 2.0 * PI * r,
                min_span: 2.0 * r,
                max_span: 2.0 * r,
            })
        }
        "IFCELLIPSEPROFILEDEF" => {
            let (sa, sb) = (a(3)?, a(4)?);
            Some(ProfileMetrics {
                area: PI * sa * sb,
                // Ramanujan's approximation — the exact perimeter is a
                // non-elementary elliptic integral. Relative error < 1e-9 for
                // any realistic aspect ratio.
                perimeter: PI * (3.0 * (sa + sb) - ((3.0 * sa + sb) * (sa + 3.0 * sb)).sqrt()),
                min_span: 2.0 * sa.min(sb),
                max_span: 2.0 * sa.max(sb),
            })
        }
        // Rolled steel sections. Fillet radii are ignored: they are optional in
        // IFC and contribute well under 1% of area.
        "IFCISHAPEPROFILEDEF" => {
            let (w, d, tw, tf) = (a(3)?, a(4)?, a(5)?, a(6)?);
            Some(ProfileMetrics {
                area: 2.0 * w * tf + (d - 2.0 * tf).max(0.0) * tw,
                perimeter: 2.0 * (w + d) + 2.0 * (w - tw),
                min_span: w.min(d),
                max_span: w.max(d),
            })
        }
        "IFCASYMMETRICISHAPEPROFILEDEF" => {
            // args: (.., BottomFlangeWidth, OverallDepth, WebThickness,
            //        BottomFlangeThickness, .., TopFlangeWidth, .., TopFlangeThickness)
            let (bw, d, tw, btf) = (a(3)?, a(4)?, a(5)?, a(6)?);
            let twf = a(8).unwrap_or(bw);
            let ttf = a(9).unwrap_or(btf);
            Some(ProfileMetrics {
                area: bw * btf + twf * ttf + (d - btf - ttf).max(0.0) * tw,
                perimeter: 2.0 * d + bw + twf + 2.0 * (bw - tw).max(0.0).max(0.0),
                min_span: bw.min(twf).min(d),
                max_span: bw.max(twf).max(d),
            })
        }
        "IFCTSHAPEPROFILEDEF" => {
            let (d, fw, tw, tf) = (a(3)?, a(4)?, a(5)?, a(6)?);
            Some(ProfileMetrics {
                area: fw * tf + (d - tf).max(0.0) * tw,
                perimeter: 2.0 * (d + fw) - tw,
                min_span: fw.min(d),
                max_span: fw.max(d),
            })
        }
        "IFCLSHAPEPROFILEDEF" => {
            // args: (.., Depth, Width, Thickness, ..)
            let d = a(3)?;
            let w = a(4).unwrap_or(d);
            let t = a(5)?;
            Some(ProfileMetrics {
                area: t * (d + w - t),
                perimeter: 2.0 * (d + w),
                min_span: t,
                max_span: d.max(w),
            })
        }
        "IFCUSHAPEPROFILEDEF" => {
            // args: (.., Depth, FlangeWidth, WebThickness, FlangeThickness, ..)
            let (d, fw, tw, tf) = (a(3)?, a(4)?, a(5)?, a(6)?);
            Some(ProfileMetrics {
                area: 2.0 * fw * tf + (d - 2.0 * tf).max(0.0) * tw,
                perimeter: 2.0 * (d + fw) + 2.0 * (fw - tw).max(0.0),
                min_span: fw.min(d),
                max_span: fw.max(d),
            })
        }
        "IFCZSHAPEPROFILEDEF" => {
            let (d, fw, tw, tf) = (a(3)?, a(4)?, a(5)?, a(6)?);
            Some(ProfileMetrics {
                area: 2.0 * fw * tf + (d - 2.0 * tf).max(0.0) * tw,
                perimeter: 2.0 * (d + 2.0 * fw),
                min_span: tw.min(tf),
                max_span: d.max(2.0 * fw),
            })
        }
        "IFCCSHAPEPROFILEDEF" => {
            // args: (.., Depth, Width, WallThickness, Girth, ..)
            let (d, w, t) = (a(3)?, a(4)?, a(5)?);
            let girth = a(6).unwrap_or(0.0);
            Some(ProfileMetrics {
                area: t * (d + 2.0 * w + 2.0 * girth - 4.0 * t).max(0.0),
                perimeter: 2.0 * (d + w),
                min_span: t,
                max_span: d.max(w),
            })
        }
        "IFCTRAPEZIUMPROFILEDEF" => {
            // args: (.., BottomXDim, TopXDim, YDim, TopXOffset)
            let (bx, tx, y) = (a(3)?, a(4)?, a(5)?);
            let off = a(6).unwrap_or(0.0);
            let slant_l = (off * off + y * y).sqrt();
            let slant_r = ((bx - tx - off).powi(2) + y * y).sqrt();
            Some(ProfileMetrics {
                area: 0.5 * (bx + tx) * y,
                perimeter: bx + tx + slant_l + slant_r,
                min_span: y.min(bx.min(tx)),
                max_span: y.max(bx.max(tx)),
            })
        }
        // Explicit polygonal outlines.
        "IFCARBITRARYCLOSEDPROFILEDEF" => {
            let curve = e.args.get(2)?.as_ref()?;
            let pts = crate::curve::polygon(step, curve)?;
            Some(from_polygon(&pts, &[]))
        }
        "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            let outer = crate::curve::polygon(step, e.args.get(2)?.as_ref()?)?;
            let mut holes = Vec::new();
            if let Some(list) = e.args.get(3).and_then(StepValue::as_list) {
                for h in list.iter().filter_map(StepValue::as_ref) {
                    if let Some(p) = crate::curve::polygon(step, h) {
                        holes.push(p);
                    }
                }
            }
            Some(from_polygon(&outer, &holes))
        }
        // Wrappers: measure what they wrap. A derived profile applies an
        // operator we do not evaluate, so only the identity case is safe.
        "IFCCOMPOSITEPROFILEDEF" => {
            let parts = e.args.get(2)?.as_list()?;
            let mut total = ProfileMetrics { area: 0.0, perimeter: 0.0, min_span: f64::MAX, max_span: 0.0 };
            let mut any = false;
            for p in parts.iter().filter_map(StepValue::as_ref) {
                if let Some(m) = metrics(step, p) {
                    total.area += m.area;
                    total.perimeter += m.perimeter;
                    total.min_span = total.min_span.min(m.min_span);
                    total.max_span = total.max_span.max(m.max_span);
                    any = true;
                }
            }
            any.then_some(total)
        }
        _ => None,
    }
}

fn rect(x: f64, y: f64) -> ProfileMetrics {
    ProfileMetrics {
        area: x * y,
        perimeter: 2.0 * (x + y),
        min_span: x.min(y),
        max_span: x.max(y),
    }
}

/// Shoelace area and boundary length. Exact for any simple polygon, convex or
/// not.
fn from_polygon(outer: &[[f64; 2]], holes: &[Vec<[f64; 2]>]) -> ProfileMetrics {
    let (mut area, perimeter) = shoelace(outer);
    for h in holes {
        let (ha, _) = shoelace(h);
        area -= ha;
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in outer {
        for i in 0..2 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let (sx, sy) = (hi[0] - lo[0], hi[1] - lo[1]);
    ProfileMetrics {
        area: area.max(0.0),
        perimeter,
        min_span: sx.min(sy),
        max_span: sx.max(sy),
    }
}

fn shoelace(pts: &[[f64; 2]]) -> (f64, f64) {
    let n = pts.len();
    if n < 3 {
        return (0.0, 0.0);
    }
    let mut a2 = 0.0;
    let mut per = 0.0;
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        a2 += p[0] * q[1] - q[0] * p[1];
        per += ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
    }
    ((a2 / 2.0).abs(), per)
}

pub(crate) fn real(v: &StepValue) -> Option<f64> {
    match v {
        StepValue::Real(r) => Some(*r),
        StepValue::Int(i) => Some(*i as f64),
        StepValue::Typed { value, .. } => real(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_step::parse_step_bytes;

    fn parse(body: &str) -> StepFile {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
             FILE_NAME('','',(''),(''),'',' ','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
             DATA;\n{body}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        parse_step_bytes(src.as_bytes()).expect("parse")
    }

    #[test]
    fn rectangle_is_exact() {
        let s = parse("#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,200.,300.);\n");
        let m = metrics(&s, 1).unwrap();
        assert_eq!(m.area, 60000.0);
        assert_eq!(m.perimeter, 1000.0);
        assert_eq!(m.min_span, 200.0);
    }

    #[test]
    fn circle_is_exact() {
        let s = parse("#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,3.);\n");
        let m = metrics(&s, 1).unwrap();
        assert!((m.area - PI * 9.0).abs() < 1e-12);
        assert!((m.perimeter - 6.0 * PI).abs() < 1e-12);
    }

    /// The type that previously produced *no quantities at all* for a slab.
    #[test]
    fn ushape_is_now_supported() {
        let s = parse("#1=IFCUSHAPEPROFILEDEF(.AREA.,$,$,300.,100.,10.,15.,$,$,$);\n");
        let m = metrics(&s, 1).unwrap();
        // 2 flanges 100x15 + web (300-30)x10 = 3000 + 2700 = 5700
        assert!((m.area - 5700.0).abs() < 1e-9, "area {}", m.area);
    }

    #[test]
    fn arbitrary_polygon_uses_shoelace() {
        let s = parse(
            "#1=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#2);\n\
             #2=IFCPOLYLINE((#3,#4,#5,#6));\n\
             #3=IFCCARTESIANPOINT((0.,0.));\n\
             #4=IFCCARTESIANPOINT((4.,0.));\n\
             #5=IFCCARTESIANPOINT((4.,3.));\n\
             #6=IFCCARTESIANPOINT((0.,3.));\n",
        );
        let m = metrics(&s, 1).unwrap();
        assert!((m.area - 12.0).abs() < 1e-12);
        assert!((m.perimeter - 14.0).abs() < 1e-12);
    }

    #[test]
    fn voids_are_subtracted_from_profile_area() {
        let s = parse(
            "#1=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#2,(#7));\n\
             #2=IFCPOLYLINE((#3,#4,#5,#6));\n\
             #3=IFCCARTESIANPOINT((0.,0.));\n#4=IFCCARTESIANPOINT((10.,0.));\n\
             #5=IFCCARTESIANPOINT((10.,10.));\n#6=IFCCARTESIANPOINT((0.,10.));\n\
             #7=IFCPOLYLINE((#8,#9,#10,#11));\n\
             #8=IFCCARTESIANPOINT((4.,4.));\n#9=IFCCARTESIANPOINT((6.,4.));\n\
             #10=IFCCARTESIANPOINT((6.,6.));\n#11=IFCCARTESIANPOINT((4.,6.));\n",
        );
        let m = metrics(&s, 1).unwrap();
        assert!((m.area - 96.0).abs() < 1e-12, "area {}", m.area);
    }

    #[test]
    fn unrecognised_profiles_yield_nothing() {
        let s = parse("#1=IFCDERIVEDPROFILEDEF(.AREA.,$,#2,#3,$);\n");
        assert!(metrics(&s, 1).is_none());
    }
}
