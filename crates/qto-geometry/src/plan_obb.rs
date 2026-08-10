//! Oriented bounding rectangle of a point set projected onto the ground plane.
//!
//! World-axis extents describe an object only when the object happens to be
//! aligned with the world axes. A wall at 30 degrees has an axis-aligned box far
//! larger than itself in both plan directions, so its thickness and its run
//! cannot be read off that box at all — which is why every dimension quantity
//! taken from world extents had to be withheld.
//!
//! The minimum-area enclosing rectangle carries the orientation the object
//! actually has. By the rotating-calipers theorem that rectangle is flush with
//! an edge of the convex hull, so trying every hull edge finds it exactly; for a
//! rectangular footprint at any rotation the result is that rectangle, to
//! floating-point.

/// Sides of the minimum-area enclosing rectangle, smaller first, together with
/// the area of the convex hull the points span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanObb {
    /// Shorter side of the enclosing rectangle — an element's thickness.
    pub min_side: f64,
    /// Longer side — the distance the element runs along its own axis.
    pub max_side: f64,
    /// Area of the convex hull of the projection.
    ///
    /// Compared against `min_side * max_side` this says whether the footprint
    /// really is the rectangle, or merely fits inside it.
    pub hull_area: f64,
}

impl PlanObb {
    /// Is the footprint the enclosing rectangle itself, rather than some shape
    /// that merely fits inside it?
    pub fn is_rectangular(&self, rel_tol: f64) -> bool {
        let box_area = self.min_side * self.max_side;
        box_area > 0.0 && ((self.hull_area - box_area).abs() / box_area) < rel_tol
    }
}

/// Minimum-area enclosing rectangle of `points`, or `None` if they are collinear
/// or too few to span an area.
pub fn min_area_rect(points: &[[f64; 2]]) -> Option<PlanObb> {
    let hull = convex_hull(points)?;
    if hull.len() < 3 {
        return None;
    }
    let hull_area = shoelace(&hull);

    let n = hull.len();
    let mut best: Option<PlanObb> = None;
    for i in 0..n {
        let a = hull[i];
        let b = hull[(i + 1) % n];
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-12 {
            continue;
        }
        // Unit vectors along and across this hull edge.
        let (ux, uy) = (dx / len, dy / len);
        let (mut u_lo, mut u_hi) = (f64::MAX, f64::MIN);
        let (mut v_lo, mut v_hi) = (f64::MAX, f64::MIN);
        for p in &hull {
            let u = p[0] * ux + p[1] * uy;
            let v = -p[0] * uy + p[1] * ux;
            u_lo = u_lo.min(u);
            u_hi = u_hi.max(u);
            v_lo = v_lo.min(v);
            v_hi = v_hi.max(v);
        }
        let (w, h) = (u_hi - u_lo, v_hi - v_lo);
        let cand = PlanObb {
            min_side: w.min(h),
            max_side: w.max(h),
            hull_area,
        };
        if best
            .map(|b| cand.min_side * cand.max_side < b.min_side * b.max_side)
            .unwrap_or(true)
        {
            best = Some(cand);
        }
    }
    best
}

/// Andrew's monotone chain, counter-clockwise, without the closing repeat.
fn convex_hull(points: &[[f64; 2]]) -> Option<Vec<[f64; 2]>> {
    if points.len() < 3 {
        return None;
    }
    let mut p: Vec<[f64; 2]> = points.to_vec();
    p.sort_by(|a, b| {
        a[0].partial_cmp(&b[0])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a[1].partial_cmp(&b[1]).unwrap_or(std::cmp::Ordering::Equal))
    });
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
    if p.len() < 3 {
        return None;
    }

    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };

    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(p.len() * 2);
    for &pt in p.iter() {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], pt) <= 0.0 {
            hull.pop();
        }
        hull.push(pt);
    }
    let lower = hull.len() + 1;
    for &pt in p.iter().rev() {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], pt) <= 0.0 {
            hull.pop();
        }
        hull.push(pt);
    }
    hull.pop();
    (hull.len() >= 3).then_some(hull)
}

fn shoelace(poly: &[[f64; 2]]) -> f64 {
    let n = poly.len();
    let mut acc = 0.0;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        acc += a[0] * b[1] - b[0] * a[1];
    }
    (acc * 0.5).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rotate(pts: &[[f64; 2]], deg: f64) -> Vec<[f64; 2]> {
        let (s, c) = deg.to_radians().sin_cos();
        pts.iter()
            .map(|p| [p[0] * c - p[1] * s, p[0] * s + p[1] * c])
            .collect()
    }

    /// The whole point: a rotated rectangle must still measure as itself.
    #[test]
    fn a_rotated_rectangle_keeps_its_own_sides() {
        let rect = [[0.0, 0.0], [5.0, 0.0], [5.0, 0.3], [0.0, 0.3]];
        for deg in [0.0, 17.0, 30.0, 45.0, 63.4, 90.0, 123.0] {
            let obb = min_area_rect(&rotate(&rect, deg)).expect("obb");
            assert!(
                (obb.min_side - 0.3).abs() < 1e-9 && (obb.max_side - 5.0).abs() < 1e-9,
                "at {deg} deg got {obb:?}"
            );
            assert!(obb.is_rectangular(1e-9), "at {deg} deg: {obb:?}");
        }
    }

    /// An axis-aligned box is the easy case and must not regress.
    #[test]
    fn axis_aligned_matches_the_axis_aligned_box() {
        let obb = min_area_rect(&[[1.0, 2.0], [4.0, 2.0], [4.0, 8.0], [1.0, 8.0]]).unwrap();
        assert_eq!(obb.min_side, 3.0);
        assert_eq!(obb.max_side, 6.0);
        assert_eq!(obb.hull_area, 18.0);
    }

    /// An L-shape fits inside a rectangle it does not fill, and must say so —
    /// otherwise its enclosing box would be reported as its dimensions.
    #[test]
    fn a_non_rectangular_footprint_is_flagged() {
        let l = [
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 1.0],
            [1.0, 1.0],
            [1.0, 4.0],
            [0.0, 4.0],
        ];
        let obb = min_area_rect(&l).unwrap();
        assert!(!obb.is_rectangular(1e-6), "{obb:?}");
        assert!((obb.hull_area - 11.5).abs() < 1e-9, "{obb:?}");
    }

    #[test]
    fn collinear_points_have_no_rectangle() {
        assert!(min_area_rect(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]).is_none());
    }

    /// A circle's minimum rectangle is its bounding square, whatever the
    /// sampling rotation — a sanity check that no hull edge is favoured.
    #[test]
    fn a_circle_measures_as_its_diameter() {
        let pts: Vec<[f64; 2]> = (0..360)
            .map(|i| {
                let a = (i as f64).to_radians();
                [2.0 * a.cos(), 2.0 * a.sin()]
            })
            .collect();
        let obb = min_area_rect(&pts).unwrap();
        assert!((obb.min_side - 4.0).abs() < 1e-3, "{obb:?}");
        assert!((obb.max_side - 4.0).abs() < 1e-3, "{obb:?}");
    }
}
