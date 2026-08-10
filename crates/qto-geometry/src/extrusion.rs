//! Exact metrics for `IfcExtrudedAreaSolid`.
//!
//! A prism swept from a planar profile along a vector **v** has volume
//! `area × |v · n̂|`, where `n̂` is the profile's unit normal. That is exact, so
//! no kernel is involved.
//!
//! The `|v · n̂|` factor is the part the previous implementation omitted: it read
//! the extrusion *depth* and ignored the extrusion *direction* entirely
//! (`IfcExtrudedAreaSolid.ExtrudedDirection`, argument 2). For the common
//! vertical sweep the direction is `(0,0,1)` and the factor is 1, which is why
//! it went unnoticed; for an oblique sweep the volume was over-reported by
//! `1/cos θ`, and for a horizontally-extruded slab the thickness and length
//! quantities were swapped outright.

use ifc_step::{EntityId, StepFile, StepValue};

use crate::profile::{metrics as profile_metrics, real, ProfileMetrics};

#[derive(Debug, Clone, PartialEq)]
pub struct ExtrusionMetrics {
    /// Enclosed volume, exact.
    pub volume: f64,
    /// Lateral (side) area only — the swept faces, excluding the two caps.
    pub lateral_area: f64,
    /// Total surface area including both caps.
    pub total_area: f64,
    /// Perpendicular sweep distance: `depth × |d · n̂|`.
    pub height: f64,
    /// Sweep distance as declared, before the direction factor.
    pub depth: f64,
    /// The swept profile.
    pub profile: ProfileMetrics,
}

/// Metrics for an `IfcExtrudedAreaSolid`, or `None` if its profile is not one
/// whose area is exactly known.
pub fn metrics_for_extrusion(step: &StepFile, solid_id: EntityId) -> Option<ExtrusionMetrics> {
    let e = step.entities.get(&solid_id)?;
    if e.entity_name != "IFCEXTRUDEDAREASOLID" {
        return None;
    }
    let profile = profile_metrics(step, e.args.first()?.as_ref()?)?;
    let depth = real(e.args.get(3)?)?;
    if !(depth.is_finite() && depth > 0.0) {
        return None;
    }

    // The profile lies in the XY plane of the solid's own placement, so its
    // normal is that placement's local Z. ExtrudedDirection is expressed in the
    // same local system, which makes the factor simply |d_z| once normalised.
    let dir = e
        .args
        .get(2)
        .and_then(StepValue::as_ref)
        .and_then(|id| direction(step, id))
        .unwrap_or([0.0, 0.0, 1.0]);
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if len < 1e-12 {
        return None;
    }
    let cos = (dir[2] / len).abs();
    if cos < 1e-12 {
        // Sweeping parallel to the profile plane encloses no volume.
        return None;
    }

    let height = depth * cos;
    let lateral_area = profile.perimeter * depth;
    Some(ExtrusionMetrics {
        volume: profile.area * height,
        lateral_area,
        total_area: lateral_area + 2.0 * profile.area,
        height,
        depth,
        profile,
    })
}

fn direction(step: &StepFile, id: EntityId) -> Option<[f64; 3]> {
    let c = step.entities.get(&id)?.args.first()?.as_list()?;
    Some([
        real(c.first()?)?,
        c.get(1).and_then(real).unwrap_or(0.0),
        c.get(2).and_then(real).unwrap_or(0.0),
    ])
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

    fn wall(dir: &str) -> StepFile {
        parse(&format!(
            "#1=IFCEXTRUDEDAREASOLID(#2,$,#3,3000.);\n\
             #2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,5000.,200.);\n\
             #3=IFCDIRECTION({dir});\n"
        ))
    }

    #[test]
    fn vertical_extrusion_is_exact() {
        let m = metrics_for_extrusion(&wall("(0.,0.,1.)"), 1).unwrap();
        assert_eq!(m.volume, 5000.0 * 200.0 * 3000.0);
        assert_eq!(m.height, 3000.0);
    }

    /// The direction was previously ignored. A 60-degree sweep encloses half the
    /// volume of a vertical one, not the same.
    #[test]
    fn oblique_extrusion_uses_the_direction() {
        // dz/|d| = 0.5 exactly.
        let m = metrics_for_extrusion(&wall("(0.866025403784439,0.,0.5)"), 1).unwrap();
        let expected = 5000.0 * 200.0 * 3000.0 * 0.5;
        assert!(
            (m.volume - expected).abs() / expected < 1e-9,
            "volume {} expected {expected}",
            m.volume
        );
    }

    #[test]
    fn sweeping_parallel_to_the_profile_encloses_nothing() {
        assert!(metrics_for_extrusion(&wall("(1.,0.,0.)"), 1).is_none());
    }

    /// `OuterSurfaceArea` measured 1.8% correct with ~100% median error because
    /// it used the full lateral wrap where the caps also matter, and vice versa.
    /// Both are now available and distinct.
    #[test]
    fn lateral_and_total_area_are_distinguished() {
        let m = metrics_for_extrusion(&wall("(0.,0.,1.)"), 1).unwrap();
        assert_eq!(m.lateral_area, 2.0 * (5000.0 + 200.0) * 3000.0);
        assert_eq!(m.total_area, m.lateral_area + 2.0 * 5000.0 * 200.0);
    }

    #[test]
    fn unknown_profiles_yield_nothing() {
        let s = parse(
            "#1=IFCEXTRUDEDAREASOLID(#2,$,#3,3000.);\n\
             #2=IFCDERIVEDPROFILEDEF(.AREA.,$,$,$,$);\n\
             #3=IFCDIRECTION((0.,0.,1.));\n",
        );
        assert!(metrics_for_extrusion(&s, 1).is_none());
    }
}
