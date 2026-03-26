//! Full 4×4 affine transform extraction from IFC placement chains.
//!
//! Walks the IfcLocalPlacement → IfcAxis2Placement3D chain and builds a
//! world-space transform matrix including rotation and translation.

use ifc_step::{EntityId, StepFile, StepValue};

/// A 4×4 column-major affine transform matrix.
#[derive(Debug, Clone, Copy)]
pub struct Transform4 {
    /// Column-major storage: m[col][row]
    pub m: [[f64; 4]; 4],
}

impl Transform4 {
    pub fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Multiply self * other (self applied after other).
    pub fn mul(&self, other: &Transform4) -> Transform4 {
        let mut r = [[0.0f64; 4]; 4];
        for c in 0..4 {
            for row in 0..4 {
                r[c][row] = self.m[0][row] * other.m[c][0]
                    + self.m[1][row] * other.m[c][1]
                    + self.m[2][row] * other.m[c][2]
                    + self.m[3][row] * other.m[c][3];
            }
        }
        Transform4 { m: r }
    }

    /// Transform a 3D point.
    pub fn transform_point(&self, p: &[f64; 3]) -> [f64; 3] {
        [
            self.m[0][0] * p[0] + self.m[1][0] * p[1] + self.m[2][0] * p[2] + self.m[3][0],
            self.m[0][1] * p[0] + self.m[1][1] * p[1] + self.m[2][1] * p[2] + self.m[3][1],
            self.m[0][2] * p[0] + self.m[1][2] * p[1] + self.m[2][2] * p[2] + self.m[3][2],
        ]
    }

    /// Build a transform from axis directions and origin.
    /// `z_axis` is the local Z direction, `x_axis` is the local X direction (RefDirection).
    /// Y is derived as Z × X (right-hand rule per IFC spec).
    pub fn from_axis_and_origin(origin: [f64; 3], z_axis: [f64; 3], x_axis: [f64; 3]) -> Self {
        let z = normalize(z_axis);
        // Make x_axis orthogonal to z
        let x_raw = x_axis;
        let dot_xz = dot(&x_raw, &z);
        let x_orth = [
            x_raw[0] - dot_xz * z[0],
            x_raw[1] - dot_xz * z[1],
            x_raw[2] - dot_xz * z[2],
        ];
        let x = normalize(x_orth);
        let y = cross(&z, &x);

        Transform4 {
            m: [
                [x[0], x[1], x[2], 0.0],                // column 0 = local X axis
                [y[0], y[1], y[2], 0.0],                // column 1 = local Y axis
                [z[0], z[1], z[2], 0.0],                // column 2 = local Z axis
                [origin[0], origin[1], origin[2], 1.0], // column 3 = translation
            ],
        }
    }
}

/// Extract the full world-space transform for an IFC element by walking its
/// IfcLocalPlacement chain.
///
/// `element_id` should be the EntityId of an IfcProduct (wall, slab, etc.).
/// Returns the composed world transform (parent placements applied first).
pub fn element_world_transform(step: &StepFile, element_id: EntityId) -> Transform4 {
    let entity = match step.entities.get(&element_id) {
        Some(e) => e,
        None => return Transform4::identity(),
    };
    // ObjectPlacement is typically args[5] for IfcProduct subtypes
    let placement_id = match entity.args.get(5) {
        Some(StepValue::Ref(id)) => *id,
        _ => return Transform4::identity(),
    };
    placement_chain_transform(step, placement_id, 0)
}

/// Recursively build the world transform from an IfcLocalPlacement chain.
fn placement_chain_transform(step: &StepFile, placement_id: EntityId, depth: usize) -> Transform4 {
    if depth > 20 {
        return Transform4::identity();
    }
    let entity = match step.entities.get(&placement_id) {
        Some(e) => e,
        None => return Transform4::identity(),
    };
    if entity.entity_name != "IFCLOCALPLACEMENT" {
        return Transform4::identity();
    }

    // args[1] = RelativePlacement (IfcAxis2Placement3D)
    let local_transform = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => axis2placement3d_transform(step, *id),
        _ => Transform4::identity(),
    };

    // args[0] = PlacementRelTo (parent IfcLocalPlacement, optional)
    let parent_transform = match entity.args.first() {
        Some(StepValue::Ref(parent_id)) => placement_chain_transform(step, *parent_id, depth + 1),
        _ => Transform4::identity(),
    };

    // World = Parent * Local
    parent_transform.mul(&local_transform)
}

/// Build a transform from an IfcAxis2Placement3D entity.
fn axis2placement3d_transform(step: &StepFile, id: EntityId) -> Transform4 {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return Transform4::identity(),
    };
    if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
        return Transform4::identity();
    }

    // args[0] = Location (IfcCartesianPoint)
    let origin = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => cartesian_point(step, *id),
        _ => [0.0, 0.0, 0.0],
    };

    // args[1] = Axis (Z direction), optional — default (0,0,1)
    let z_axis = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => direction(step, *id),
        _ => [0.0, 0.0, 1.0],
    };

    // args[2] = RefDirection (X direction), optional — default (1,0,0)
    let x_axis = match entity.args.get(2) {
        Some(StepValue::Ref(id)) => direction(step, *id),
        _ => default_ref_direction(&z_axis),
    };

    Transform4::from_axis_and_origin(origin, z_axis, x_axis)
}

fn cartesian_point(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 0.0],
    };
    if entity.entity_name != "IFCCARTESIANPOINT" {
        return [0.0, 0.0, 0.0];
    }
    let coords = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 0.0],
    };
    let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

fn direction(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 1.0],
    };
    if entity.entity_name != "IFCDIRECTION" {
        return [0.0, 0.0, 1.0];
    }
    let ratios = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 1.0],
    };
    let x = ratios.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = ratios.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = ratios.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

/// Compute a default RefDirection perpendicular to the given Z axis.
fn default_ref_direction(z: &[f64; 3]) -> [f64; 3] {
    // Pick the axis least aligned with z
    let ax = z[0].abs();
    let ay = z[1].abs();
    let az = z[2].abs();
    let candidate = if ax <= ay && ax <= az {
        [1.0, 0.0, 0.0]
    } else if ay <= az {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    // Gram-Schmidt: remove z component
    let d = dot(&candidate, z);
    let x = [
        candidate[0] - d * z[0],
        candidate[1] - d * z[1],
        candidate[2] - d * z[2],
    ];
    normalize(x)
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-15 {
        return [1.0, 0.0, 0.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

fn cross(a: &[f64; 3], b: &[f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_transform() {
        let t = Transform4::identity();
        let p = t.transform_point(&[1.0, 2.0, 3.0]);
        assert!((p[0] - 1.0).abs() < 1e-10);
        assert!((p[1] - 2.0).abs() < 1e-10);
        assert!((p[2] - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_translation() {
        let t =
            Transform4::from_axis_and_origin([10.0, 20.0, 30.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let p = t.transform_point(&[1.0, 2.0, 3.0]);
        assert!((p[0] - 11.0).abs() < 1e-10);
        assert!((p[1] - 22.0).abs() < 1e-10);
        assert!((p[2] - 33.0).abs() < 1e-10);
    }

    #[test]
    fn test_90deg_rotation_z() {
        // Rotate 90° around Z: X→Y, Y→-X
        let t = Transform4::from_axis_and_origin(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0], // new X direction = world Y
        );
        let p = t.transform_point(&[1.0, 0.0, 0.0]);
        assert!((p[0] - 0.0).abs() < 1e-10);
        assert!((p[1] - 1.0).abs() < 1e-10);
        assert!((p[2] - 0.0).abs() < 1e-10);
    }
}
