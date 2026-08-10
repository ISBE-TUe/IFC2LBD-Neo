//! Read a closed 2D curve out of IFC as a polygon.
//!
//! Only forms that are *exactly* polygonal are accepted. A composite curve
//! containing arcs is refused rather than chorded into line segments, because a
//! chorded arc under-reports area and the whole point of this rebuild is that a
//! missing quantity beats a plausible wrong one.

use ifc_step::{EntityId, StepFile, StepValue};

use crate::profile::real;

/// Vertices of a closed polygon, or `None` if the curve is not exactly
/// polygonal.
pub fn polygon(step: &StepFile, curve_id: EntityId) -> Option<Vec<[f64; 2]>> {
    let e = step.entities.get(&curve_id)?;
    let pts = match e.entity_name.as_str() {
        "IFCPOLYLINE" => e
            .args
            .first()?
            .as_list()?
            .iter()
            .filter_map(StepValue::as_ref)
            .filter_map(|id| point2(step, id))
            .collect::<Vec<_>>(),

        "IFCINDEXEDPOLYCURVE" => {
            let all = point_list(step, e.args.first()?.as_ref()?)?;
            match e.args.get(1).and_then(StepValue::as_list) {
                // Segments may be IfcLineIndex (polygonal) or IfcArcIndex
                // (curved). Any arc makes the outline non-polygonal.
                Some(segments) if !segments.is_empty() => {
                    let mut ordered = Vec::new();
                    for seg in segments {
                        let (kind, idx) = match seg {
                            StepValue::Typed { type_name, value } => {
                                (type_name.to_uppercase(), value.as_list())
                            }
                            StepValue::List(_) => ("IFCLINEINDEX".to_string(), seg.as_list()),
                            _ => continue,
                        };
                        if kind.contains("ARC") {
                            return None;
                        }
                        for i in idx?.iter().filter_map(StepValue::as_int) {
                            if let Some(&p) = all.get((i - 1).max(0) as usize) {
                                ordered.push(p);
                            }
                        }
                    }
                    ordered
                }
                // No segment list: the points are the polygon, in order.
                _ => all,
            }
        }
        _ => return None,
    };

    dedup_closing_point(pts)
}

fn dedup_closing_point(mut pts: Vec<[f64; 2]>) -> Option<Vec<[f64; 2]>> {
    // Consecutive duplicates arise where segments meet; the closing repeat of
    // the first vertex would otherwise add a zero-length edge.
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
    if pts.len() > 1 {
        let (first, last) = (pts[0], pts[pts.len() - 1]);
        if (first[0] - last[0]).abs() < 1e-12 && (first[1] - last[1]).abs() < 1e-12 {
            pts.pop();
        }
    }
    (pts.len() >= 3).then_some(pts)
}

fn point2(step: &StepFile, id: EntityId) -> Option<[f64; 2]> {
    let c = step.entities.get(&id)?.args.first()?.as_list()?;
    Some([real(c.first()?)?, c.get(1).and_then(real).unwrap_or(0.0)])
}

/// `IfcCartesianPointList2D` / `3D` — coordinates inline rather than as refs.
fn point_list(step: &StepFile, id: EntityId) -> Option<Vec<[f64; 2]>> {
    let rows = step.entities.get(&id)?.args.first()?.as_list()?;
    Some(
        rows.iter()
            .filter_map(|r| {
                let c = r.as_list()?;
                Some([real(c.first()?)?, c.get(1).and_then(real).unwrap_or(0.0)])
            })
            .collect(),
    )
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
    fn indexed_polycurve_follows_segment_order() {
        let s = parse(
            "#1=IFCINDEXEDPOLYCURVE(#2,(IFCLINEINDEX((1,2,3,4,1))),$);\n\
             #2=IFCCARTESIANPOINTLIST2D(((0.,-0.24),(5.25,-0.24),(5.25,0.),(0.,0.)));\n",
        );
        let p = polygon(&s, 1).expect("polygon");
        assert_eq!(p.len(), 4, "closing repeat should be dropped: {p:?}");
    }

    /// An arc cannot be represented as a polygon; chording it would silently
    /// under-report the area.
    #[test]
    fn arcs_are_refused_rather_than_chorded() {
        let s = parse(
            "#1=IFCINDEXEDPOLYCURVE(#2,(IFCLINEINDEX((1,2)),IFCARCINDEX((2,3,4))),$);\n\
             #2=IFCCARTESIANPOINTLIST2D(((0.,0.),(5.,0.),(6.,1.),(5.,2.)));\n",
        );
        assert!(polygon(&s, 1).is_none());
    }

    #[test]
    fn polyline_reads_in_order() {
        let s = parse(
            "#1=IFCPOLYLINE((#2,#3,#4));\n\
             #2=IFCCARTESIANPOINT((0.,0.));\n\
             #3=IFCCARTESIANPOINT((1.,0.));\n\
             #4=IFCCARTESIANPOINT((0.,1.));\n",
        );
        assert_eq!(polygon(&s, 1).unwrap().len(), 3);
    }
}
