//! OpenCASCADE-backed metrics, for the geometry the exact analytic paths refuse.
//!
//! The analytic paths in [`crate::extrusion`] and [`crate::polyhedron`] are
//! exact for what they cover, and cover most of a typical model. They refuse
//! three things, and this module exists for those:
//!
//! * **Curved outlines** — a profile whose boundary contains arcs. Chording an
//!   arc into line segments under-reports its area, so [`crate::curve`] returns
//!   `None` rather than approximating.
//! * **Booleans** — `IfcBooleanResult` / `IfcBooleanClippingResult`. The volume
//!   of a clipped solid is not the volume of its first operand.
//! * **Revolutions and sweeps** — `IfcRevolvedAreaSolid`, `IfcSweptDiskSolid`
//!   and the directrix forms, whose volumes are not `area x depth`.
//!
//! OCCT integrates over the real surfaces rather than a tessellation, so a
//! cylinder yields exactly `pi*r^2*h`. Verified against native and
//! wasm32-unknown-unknown builds, which agree bit for bit.
//!
//! # Threading
//!
//! `cadrum::Solid` is `Send` but **not `Sync`**. That is sufficient: solids are
//! constructed and consumed inside one element's computation and never shared,
//! so only `f64` results cross a thread boundary.
//!
//! # WASM
//!
//! Under `wasm32` OCCT's C++ static constructors do not run on their own. Call
//! [`init`] once before anything else here, or the first call traps.

use cadrum::{DVec3, Edge, Solid};

/// Run OCCT's static constructors. Idempotent; required exactly once on wasm32
/// before any other call in this module, and a no-op elsewhere.
pub fn init() {
    #[cfg(target_arch = "wasm32")]
    {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            cadrum::__anchor_wasi_stub();
            unsafe extern "C" {
                fn __wasm_call_ctors();
            }
            unsafe { __wasm_call_ctors() };
        });
    }
}

/// What OCCT could establish about a solid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolidMetrics {
    pub volume: f64,
    pub surface_area: f64,
}

/// Why OCCT could not measure a solid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcctError {
    /// A profile could not be turned into a closed wire.
    BadProfile(String),
    /// The kernel reported failure — a boolean that did not resolve, a sweep
    /// that could not be built.
    KernelFailed(String),
    /// The kernel returned a result that cannot be a solid's measurement.
    ImplausibleResult,
}

/// Volume and surface area of a solid, or `Err` if the kernel could not
/// establish them.
///
/// The plausibility check is deliberate. OCCT booleans do fail on
/// exporter-produced geometry, and a failed boolean can return a degenerate or
/// empty shape whose volume reads as `0`. Emitting that would be worse than
/// emitting nothing, so a non-finite or non-positive result is refused.
pub fn measure(solid: &Solid) -> Result<SolidMetrics, OcctError> {
    let volume = solid.volume();
    let surface_area = solid.area();
    if !volume.is_finite() || volume <= 0.0 || !surface_area.is_finite() || surface_area <= 0.0 {
        return Err(OcctError::ImplausibleResult);
    }
    Ok(SolidMetrics {
        volume,
        surface_area,
    })
}

/// Build a prism by sweeping a closed polygon along a vector.
pub fn extrude_polygon(outline: &[[f64; 2]], direction: [f64; 3]) -> Result<Solid, OcctError> {
    if outline.len() < 3 {
        return Err(OcctError::BadProfile("fewer than 3 vertices".into()));
    }
    let pts: Vec<DVec3> = outline
        .iter()
        .map(|&[x, y]| DVec3::new(x, y, 0.0))
        .collect();
    let profile = Edge::polygon(&pts).map_err(|e| OcctError::BadProfile(e.to_string()))?;
    Solid::extrude(
        &profile,
        DVec3::new(direction[0], direction[1], direction[2]),
    )
    .map_err(|e| OcctError::KernelFailed(e.to_string()))
}

/// Sweep a circle — the case [`crate::curve`] refuses, since chording an arc
/// under-reports its area.
pub fn extrude_circle(radius: f64, direction: [f64; 3]) -> Result<Solid, OcctError> {
    if !(radius.is_finite() && radius > 0.0) {
        return Err(OcctError::BadProfile("non-positive radius".into()));
    }
    let profile = [Edge::circle(radius, DVec3::Z)
        .map_err(|e| OcctError::BadProfile(e.to_string()))?];
    Solid::extrude(
        &profile,
        DVec3::new(direction[0], direction[1], direction[2]),
    )
    .map_err(|e| OcctError::KernelFailed(e.to_string()))
}

/// Subtract cutters from a host — the opening/clipping case.
///
/// A cutter that consumes the host entirely leaves nothing, and the kernel
/// reports that as a failure rather than handing back an empty solid. Either
/// way no quantity is emitted: the useful property is that "nothing left" never
/// reaches output as a volume of zero.
pub fn subtract(host: &Solid, cutters: &[Solid]) -> Result<Solid, OcctError> {
    let mut out = host.clone();
    for c in cutters {
        out = (&out - c)
            .build()
            .map_err(|e| OcctError::KernelFailed(e.to_string()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn cylinder_volume_is_analytic_not_tessellated() {
        init();
        let s = extrude_circle(3.0, [0.0, 0.0, 10.0]).expect("cylinder");
        let m = measure(&s).expect("measurable");
        // The whole reason this module exists: no tessellation reproduces this.
        assert!(
            (m.volume - PI * 9.0 * 10.0).abs() < 1e-9,
            "volume {}",
            m.volume
        );
    }

    #[test]
    fn oblique_extrusion_uses_the_direction_vector() {
        init();
        let sq = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        let s = extrude_polygon(&sq, [3.0, 1.0, 8.0]).expect("prism");
        let m = measure(&s).expect("measurable");
        assert!((m.volume - 4.0 * 8.0).abs() < 1e-9, "volume {}", m.volume);
    }

    #[test]
    fn subtraction_removes_the_cutter_volume() {
        init();
        let wall = extrude_polygon(
            &[[0.0, 0.0], [5.0, 0.0], [5.0, 0.2], [0.0, 0.2]],
            [0.0, 0.0, 3.0],
        )
        .unwrap();
        let opening = extrude_polygon(
            &[[1.0, -0.1], [3.0, -0.1], [3.0, 0.3], [1.0, 0.3]],
            [0.0, 0.0, 2.0],
        )
        .unwrap();
        let net = subtract(&wall, std::slice::from_ref(&opening)).unwrap();
        let m = measure(&net).unwrap();
        // 5 x 0.2 x 3 minus the 2 x 0.2 x 2 that lies inside the wall.
        assert!((m.volume - (3.0 - 0.8)).abs() < 1e-9, "volume {}", m.volume);
    }

    /// A cutter that swallows the host must not yield a zero quantity. The
    /// kernel surfaces this as an error, which is the outcome we want — the
    /// point is that it cannot be mistaken for a measured volume.
    #[test]
    fn a_fully_consumed_host_is_refused_rather_than_reported_as_zero() {
        init();
        let host = extrude_polygon(
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            [0.0, 0.0, 1.0],
        )
        .unwrap();
        // A cutter that swallows the host leaves nothing measurable.
        let big = extrude_polygon(
            &[[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]],
            [0.0, 0.0, 10.0],
        )
        .unwrap();
        match subtract(&host, std::slice::from_ref(&big)) {
            Err(OcctError::KernelFailed(_)) => {}
            Ok(s) => assert_eq!(
                measure(&s),
                Err(OcctError::ImplausibleResult),
                "an empty result must not measure as a real solid"
            ),
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn bad_profiles_are_refused() {
        init();
        assert!(matches!(
            extrude_polygon(&[[0.0, 0.0], [1.0, 0.0]], [0.0, 0.0, 1.0]),
            Err(OcctError::BadProfile(_))
        ));
        assert!(matches!(
            extrude_circle(-1.0, [0.0, 0.0, 1.0]),
            Err(OcctError::BadProfile(_))
        ));
    }
}
