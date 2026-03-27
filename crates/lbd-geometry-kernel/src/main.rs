use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chijin::{Face, Shape};
use clap::Parser;
use glam::{DMat4, DVec2, DVec3, DVec4};
use ifc_step::{parse_step_file, EntityId, RawEntity, StepValue};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};

/// Compute axis-aligned bounding boxes for a batch of entity IDs using IfcOpenShell via PyO3.
/// Opens the IFC file once in tessellation mode (no BRep), returns entity_id → [minX,minY,minZ,maxX,maxY,maxZ].
/// Returns empty map if ifcopenshell is not installed or no entities succeed.
fn compute_bboxes_via_ifcopenshell(
    ifc_path: &Path,
    entity_ids: &[EntityId],
) -> HashMap<EntityId, [f64; 6]> {
    if entity_ids.is_empty() {
        return HashMap::new();
    }
    let ifc_path_str = ifc_path.to_string_lossy().into_owned();
    let ids_i64: Vec<i64> = entity_ids.iter().map(|&id| id as i64).collect();

    let result = Python::with_gil(|py| -> PyResult<HashMap<EntityId, [f64; 6]>> {
        let code = r#"
import ifcopenshell
import ifcopenshell.geom

def compute_bboxes(ifc_path, entity_ids):
    ifc = ifcopenshell.open(ifc_path)
    settings = ifcopenshell.geom.settings()
    settings.set(settings.USE_WORLD_COORDS, True)
    id_set = set(entity_ids)
    elements = [e for eid in id_set if (e := ifc.by_id(eid)) is not None]
    if not elements:
        return {}
    out = {}
    iterator = ifcopenshell.geom.iterator(settings, ifc, include=elements)
    if iterator.initialize():
        while True:
            shape = iterator.get()
            verts = shape.geometry.verts
            if len(verts) >= 3:
                xs = verts[0::3]
                ys = verts[1::3]
                zs = verts[2::3]
                out[shape.id] = [min(xs), min(ys), min(zs), max(xs), max(ys), max(zs)]
            if not iterator.next():
                break
    return out
"#;
        let module = PyModule::from_code_bound(py, code, "ifs_bbox", "ifs_bbox")?;
        let py_ids = pyo3::types::PyList::new_bound(py, &ids_i64);
        let py_result = module
            .getattr("compute_bboxes")?
            .call1((ifc_path_str.as_str(), py_ids))?;
        let bbox_map: HashMap<i64, Vec<f64>> = py_result.extract()?;
        let mut out: HashMap<EntityId, [f64; 6]> = HashMap::new();
        for (id_i64, bbox_vec) in bbox_map {
            if bbox_vec.len() == 6 {
                out.insert(
                    id_i64 as EntityId,
                    [
                        bbox_vec[0],
                        bbox_vec[1],
                        bbox_vec[2],
                        bbox_vec[3],
                        bbox_vec[4],
                        bbox_vec[5],
                    ],
                );
            }
        }
        Ok(out)
    });

    result.unwrap_or_default()
}

/// Check whether two AABBs [minX,minY,minZ,maxX,maxY,maxZ] overlap within `tolerance`.
fn bboxes_overlap(a: &[f64; 6], b: &[f64; 6], tolerance: f64) -> bool {
    a[0] - tolerance <= b[3]
        && b[0] - tolerance <= a[3]
        && a[1] - tolerance <= b[4]
        && b[1] - tolerance <= a[4]
        && a[2] - tolerance <= b[5]
        && b[2] - tolerance <= a[5]
}

/// Build BRep shapes for a batch of entity IDs using IfcOpenShell via PyO3.
/// Opens the IFC file once in Python, processes all IDs, returns entity_id → BREP text.
/// Returns empty map if ifcopenshell is not installed or no entities succeed.
fn build_shapes_via_ifcopenshell(
    ifc_path: &Path,
    entity_ids: &[EntityId],
) -> HashMap<EntityId, Shape> {
    if entity_ids.is_empty() {
        return HashMap::new();
    }
    let ifc_path_str = ifc_path.to_string_lossy().into_owned();
    let ids_i64: Vec<i64> = entity_ids.iter().map(|&id| id as i64).collect();

    let result = Python::with_gil(|py| -> PyResult<HashMap<EntityId, Shape>> {
        let code = r#"
import ifcopenshell
import ifcopenshell.geom

def build_breps(ifc_path, entity_ids):
    ifc = ifcopenshell.open(ifc_path)
    settings = ifcopenshell.geom.settings()
    settings.set(settings.USE_BREP_DATA, True)
    settings.set(settings.USE_WORLD_COORDS, True)
    out = {}
    for eid in entity_ids:
        element = ifc.by_id(eid)
        if element is None:
            continue
        try:
            shape = ifcopenshell.geom.create_shape(settings, element)
            out[eid] = shape.geometry.brep_data
        except Exception:
            pass
    return out
"#;
        let module = PyModule::from_code_bound(py, code, "ifs_geom", "ifs_geom")?;
        let py_ids = pyo3::types::PyList::new_bound(py, &ids_i64);
        let py_result = module
            .getattr("build_breps")?
            .call1((ifc_path_str.as_str(), py_ids))?;
        let brep_map: HashMap<i64, String> = py_result.extract()?;
        let mut shapes: HashMap<EntityId, Shape> = HashMap::new();
        for (id_i64, brep_text) in brep_map {
            if let Ok(shape) = Shape::read_brep_text(&mut Cursor::new(brep_text.as_bytes())) {
                shapes.insert(id_i64 as EntityId, shape);
            }
        }
        Ok(shapes)
    });

    result.unwrap_or_default()
}

#[derive(Debug, Parser)]
#[command(name = "lbd-geometry-kernel")]
#[command(about = "Native OCC batch kernel for ifc2lbd exact topology checks")]
struct Args {
    /// Optional override for directory containing per-entity BRep files.
    /// Defaults to "<ifc_path>.occ-cache".
    #[arg(long = "brep-cache-dir")]
    brep_cache_dir: Option<PathBuf>,

    /// Prebuild BRep cache from IFC for all product-like entities and exit.
    #[arg(long = "prebuild-cache-from-ifc")]
    prebuild_cache_from_ifc: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct BatchPair {
    left: u64,
    right: u64,
}

#[derive(Debug, Deserialize)]
struct BatchRequest {
    ifc_path: String,
    tolerance: f64,
    pairs: Vec<BatchPair>,
    /// Pre-computed bboxes [xmin,ymin,zmin,xmax,ymax,zmax] from CLI for elements OCC can't build.
    /// When present, the kernel uses these directly instead of calling IfcOpenShell.
    #[serde(default)]
    fallback_bboxes: std::collections::HashMap<u64, Vec<f64>>,
}

#[derive(Debug, Serialize)]
struct BatchPairResponse {
    left: u64,
    right: u64,
    intersects: bool,
    touches_within_tolerance: bool,
    minimum_distance: Option<f64>,
    interface: Option<InterfaceResponse>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct InterfaceResponse {
    interface_id: u64,
    shared_boundary_area: Option<f64>,
}

#[derive(Debug, Serialize)]
struct BatchResponse {
    results: Vec<BatchPairResponse>,
}

fn shape_path(cache_dir: &Path, entity_id: u64) -> PathBuf {
    cache_dir.join(format!("{entity_id}.brepbin"))
}

/// Path for bbox-simplified (box) shapes — used for tessellated geometry from IfcOpenShell.
/// These use bbox overlap in analyze_pair instead of full OCC boolean.
fn box_path(cache_dir: &Path, entity_id: u64) -> PathBuf {
    cache_dir.join(format!("{entity_id}.boxbin"))
}

fn load_shape(cache_dir: &Path, entity_id: u64) -> Result<Shape> {
    let path = shape_path(cache_dir, entity_id);
    let mut file = File::open(&path)
        .with_context(|| format!("failed to open BRep file {}", path.display()))?;
    Shape::read_brep_bin(&mut file)
        .map_err(|error| anyhow::anyhow!("failed to read BRep {}: {error}", path.display()))
}

fn load_box_shape(cache_dir: &Path, entity_id: u64) -> Result<Shape> {
    let path = box_path(cache_dir, entity_id);
    let mut file = File::open(&path)
        .with_context(|| format!("failed to open box BRep file {}", path.display()))?;
    Shape::read_brep_bin(&mut file)
        .map_err(|error| anyhow::anyhow!("failed to read box BRep {}: {error}", path.display()))
}

fn interface_id_for_pair(left: u64, right: u64) -> u64 {
    // Deterministic synthetic identifier in a reserved high range.
    let (a, b) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    9_000_000_000_000_000_000_u64 ^ (a.wrapping_mul(1_099_511_628_211) ^ b)
}

/// Tolerance for bbox-based touching test (1cm).
/// Used when either shape is a tessellated/bbox-simplified element (stored as .boxbin).
const BOX_TOUCH_TOL: f64 = 0.01;

fn bboxes_touch(a: &[f64; 6], b: &[f64; 6], tol: f64) -> bool {
    a[0] - tol <= b[3] + tol
        && b[0] - tol <= a[3] + tol
        && a[1] - tol <= b[4] + tol
        && b[1] - tol <= a[4] + tol
        && a[2] - tol <= b[5] + tol
        && b[2] - tol <= a[5] + tol
}

/// Load shape for pair analysis. Returns (shape, is_precise) where is_precise=false means
/// the shape is a bbox-simplified box (tessellated geometry) and requires bbox-only test.
fn load_pair_shape(cache_dir: &Path, entity_id: u64) -> Option<(Shape, bool)> {
    // Try OCC-native (precise) first
    if let Ok(s) = load_shape(cache_dir, entity_id) {
        return Some((s, true));
    }
    // Fall back to bbox-simplified box
    if let Ok(s) = load_box_shape(cache_dir, entity_id) {
        return Some((s, false));
    }
    None
}

fn analyze_pair(cache_dir: &Path, left: u64, right: u64, tolerance: f64) -> BatchPairResponse {
    let no_result = || BatchPairResponse {
        left,
        right,
        intersects: false,
        touches_within_tolerance: false,
        minimum_distance: None,
        interface: None,
        error: None,
    };

    let ((sl, sl_precise), (sr, sr_precise)) = match (
        load_pair_shape(cache_dir, left),
        load_pair_shape(cache_dir, right),
    ) {
        (Some(l), Some(r)) => (l, r),
        _ => return no_result(),
    };

    // If BOTH shapes are OCC-native (precise), run full OCC boolean for exact result.
    // OCC-native shapes come from IfcExtrudedAreaSolid, IfcBooleanResult, etc. — they have
    // proper geometric surfaces and typically few faces (fast boolean).
    if sl_precise && sr_precise {
        return match sl.intersect(&sr) {
            Ok(common) => {
                let is_null = common.shape.is_null();
                let volume = common.shape.volume().abs();
                let intersects = !is_null && volume > tolerance;
                let touches = !is_null && !intersects;
                BatchPairResponse {
                    left,
                    right,
                    intersects,
                    touches_within_tolerance: touches,
                    minimum_distance: None,
                    interface: touches.then(|| InterfaceResponse {
                        interface_id: interface_id_for_pair(left, right),
                        shared_boundary_area: None,
                    }),
                    error: None,
                }
            }
            Err(error) => BatchPairResponse {
                left,
                right,
                intersects: false,
                touches_within_tolerance: false,
                minimum_distance: None,
                interface: None,
                error: Some(format!("boolean intersection failed: {error}")),
            },
        };
    }

    // At least one shape is a tessellated/bbox-simplified box. Use OCC precise 3D bbox
    // for touch detection — faster than full boolean and accounts for rotation.
    // This is an approximation: for diagonal bboxes some false positives may occur.
    match (sl.bounding_box(), sr.bounding_box()) {
        (Some(bl), Some(br)) => {
            let touches = bboxes_touch(&bl, &br, BOX_TOUCH_TOL);
            BatchPairResponse {
                left,
                right,
                intersects: false,
                touches_within_tolerance: touches,
                minimum_distance: None,
                interface: touches.then(|| InterfaceResponse {
                    interface_id: interface_id_for_pair(left, right),
                    shared_boundary_area: None,
                }),
                error: None,
            }
        }
        _ => no_result(),
    }
}

fn analyze_pair_in_memory(
    shapes: &HashMap<EntityId, (Shape, bool)>,
    left: u64,
    right: u64,
    tolerance: f64,
) -> BatchPairResponse {
    let no_result = || BatchPairResponse {
        left,
        right,
        intersects: false,
        touches_within_tolerance: false,
        minimum_distance: None,
        interface: None,
        error: None,
    };

    let ((sl, sl_precise), (sr, sr_precise)) = match (shapes.get(&left), shapes.get(&right)) {
        (Some(l), Some(r)) => (l, r),
        _ => return no_result(),
    };

    if *sl_precise && *sr_precise {
        return match sl.intersect(sr) {
            Ok(common) => {
                let is_null = common.shape.is_null();
                let volume = common.shape.volume().abs();
                let intersects = !is_null && volume > tolerance;
                let touches = !is_null && !intersects;
                BatchPairResponse {
                    left,
                    right,
                    intersects,
                    touches_within_tolerance: touches,
                    minimum_distance: None,
                    interface: touches.then(|| InterfaceResponse {
                        interface_id: interface_id_for_pair(left, right),
                        shared_boundary_area: None,
                    }),
                    error: None,
                }
            }
            Err(error) => BatchPairResponse {
                left,
                right,
                intersects: false,
                touches_within_tolerance: false,
                minimum_distance: None,
                interface: None,
                error: Some(format!("boolean intersection failed: {error}")),
            },
        };
    }

    match (sl.bounding_box(), sr.bounding_box()) {
        (Some(bl), Some(br)) => {
            let touches = bboxes_touch(&bl, &br, BOX_TOUCH_TOL);
            BatchPairResponse {
                left,
                right,
                intersects: false,
                touches_within_tolerance: touches,
                minimum_distance: None,
                interface: touches.then(|| InterfaceResponse {
                    interface_id: interface_id_for_pair(left, right),
                    shared_boundary_area: None,
                }),
                error: None,
            }
        }
        _ => no_result(),
    }
}

fn default_cache_dir(ifc_path: &str) -> PathBuf {
    PathBuf::from(format!("{ifc_path}.occ-cache"))
}

fn identity_4() -> DMat4 {
    DMat4::from_cols(
        DVec4::new(1.0, 0.0, 0.0, 0.0),
        DVec4::new(0.0, 1.0, 0.0, 0.0),
        DVec4::new(0.0, 0.0, 1.0, 0.0),
        DVec4::new(0.0, 0.0, 0.0, 1.0),
    )
}

fn transform_point(m: DMat4, p: DVec3) -> DVec3 {
    let v = m * DVec4::new(p.x, p.y, p.z, 1.0);
    DVec3::new(v.x, v.y, v.z)
}

fn transform_vector(m: DMat4, v: DVec3) -> DVec3 {
    let out = m * DVec4::new(v.x, v.y, v.z, 0.0);
    DVec3::new(out.x, out.y, out.z)
}

fn normalize_or(v: DVec3, fallback: DVec3) -> DVec3 {
    if v.length() > 1e-12 {
        v.normalize()
    } else {
        fallback
    }
}

fn as_ref_id(value: Option<&StepValue>) -> Option<EntityId> {
    value.and_then(StepValue::as_ref)
}

fn as_real(value: Option<&StepValue>) -> Option<f64> {
    match value {
        Some(StepValue::Real(v)) => Some(*v),
        Some(StepValue::Int(v)) => Some(*v as f64),
        Some(StepValue::Typed { value, .. }) => as_real(Some(value.as_ref())),
        _ => None,
    }
}

fn as_bool(value: Option<&StepValue>) -> Option<bool> {
    value.and_then(StepValue::as_bool)
}

fn as_int(val: Option<&StepValue>) -> Result<i64> {
    match val {
        Some(StepValue::Int(i)) => Ok(*i),
        Some(StepValue::Real(r)) => Ok(*r as i64),
        other => anyhow::bail!("expected Int, got {:?}", other),
    }
}

fn refs_from_list(value: Option<&StepValue>) -> Vec<EntityId> {
    match value {
        Some(StepValue::List(items)) => items.iter().filter_map(StepValue::as_ref).collect(),
        Some(StepValue::Ref(id)) => vec![*id],
        _ => Vec::new(),
    }
}

struct IfcGeometryBuilder<'a> {
    entities: &'a HashMap<EntityId, RawEntity>,
    local_placement_cache: HashMap<EntityId, DMat4>,
}

impl<'a> IfcGeometryBuilder<'a> {
    fn new(entities: &'a HashMap<EntityId, RawEntity>) -> Self {
        Self {
            entities,
            local_placement_cache: HashMap::new(),
        }
    }

    fn entity(&self, id: EntityId) -> Result<&RawEntity> {
        self.entities
            .get(&id)
            .with_context(|| format!("missing STEP entity #{id}"))
    }

    fn cartesian_point(&self, id: EntityId) -> Result<DVec3> {
        let entity = self.entity(id)?;
        if entity.entity_name != "IFCCARTESIANPOINT" {
            anyhow::bail!(
                "#{id} is {}, expected IFCCARTESIANPOINT",
                entity.entity_name
            );
        }
        let Some(StepValue::List(coords)) = entity.args.first() else {
            anyhow::bail!("IFCCARTESIANPOINT #{id} has invalid coords");
        };
        let x = as_real(coords.first()).unwrap_or(0.0);
        let y = as_real(coords.get(1)).unwrap_or(0.0);
        let z = as_real(coords.get(2)).unwrap_or(0.0);
        Ok(DVec3::new(x, y, z))
    }

    fn cartesian_point_2d(&self, id: EntityId) -> Result<DVec2> {
        let p = self.cartesian_point(id)?;
        Ok(DVec2::new(p.x, p.y))
    }

    fn direction3(&self, id: EntityId) -> Result<DVec3> {
        let entity = self.entity(id)?;
        if entity.entity_name != "IFCDIRECTION" {
            anyhow::bail!("#{id} is {}, expected IFCDIRECTION", entity.entity_name);
        }
        let Some(StepValue::List(vals)) = entity.args.first() else {
            anyhow::bail!("IFCDIRECTION #{id} has invalid ratios");
        };
        let x = as_real(vals.first()).unwrap_or(0.0);
        let y = as_real(vals.get(1)).unwrap_or(0.0);
        let z = as_real(vals.get(2)).unwrap_or(0.0);
        Ok(normalize_or(DVec3::new(x, y, z), DVec3::new(1.0, 0.0, 0.0)))
    }

    fn axis2placement3d_matrix(&self, id: EntityId) -> Result<DMat4> {
        let entity = self.entity(id)?;
        if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
            anyhow::bail!(
                "#{id} is {}, expected IFCAXIS2PLACEMENT3D",
                entity.entity_name
            );
        }
        let origin = self.cartesian_point(
            as_ref_id(entity.args.first())
                .with_context(|| format!("IFCAXIS2PLACEMENT3D #{id} missing location"))?,
        )?;
        let z = match as_ref_id(entity.args.get(1)) {
            Some(axis) => self.direction3(axis)?,
            None => DVec3::new(0.0, 0.0, 1.0),
        };
        let x_guess = match as_ref_id(entity.args.get(2)) {
            Some(axis) => self.direction3(axis)?,
            None => DVec3::new(1.0, 0.0, 0.0),
        };
        let x_projected = x_guess - z * x_guess.dot(z);
        let x = normalize_or(x_projected, DVec3::new(1.0, 0.0, 0.0));
        let y = normalize_or(z.cross(x), DVec3::new(0.0, 1.0, 0.0));
        let x = normalize_or(y.cross(z), x);
        Ok(DMat4::from_cols(
            DVec4::new(x.x, x.y, x.z, 0.0),
            DVec4::new(y.x, y.y, y.z, 0.0),
            DVec4::new(z.x, z.y, z.z, 0.0),
            DVec4::new(origin.x, origin.y, origin.z, 1.0),
        ))
    }

    fn axis2placement2d(&self, id: EntityId) -> Result<(DVec2, DVec2, DVec2)> {
        let entity = self.entity(id)?;
        if entity.entity_name != "IFCAXIS2PLACEMENT2D" {
            anyhow::bail!(
                "#{id} is {}, expected IFCAXIS2PLACEMENT2D",
                entity.entity_name
            );
        }
        let origin = self.cartesian_point_2d(
            as_ref_id(entity.args.first())
                .with_context(|| format!("IFCAXIS2PLACEMENT2D #{id} missing location"))?,
        )?;
        let x = match as_ref_id(entity.args.get(1)) {
            Some(dir_id) => {
                let d3 = self.direction3(dir_id)?;
                normalize_or(DVec3::new(d3.x, d3.y, 0.0), DVec3::new(1.0, 0.0, 0.0))
            }
            None => DVec3::new(1.0, 0.0, 0.0),
        };
        let x2 = DVec2::new(x.x, x.y);
        let y2 = DVec2::new(-x2.y, x2.x);
        Ok((origin, x2, y2))
    }

    fn local_placement_matrix(&mut self, id: EntityId) -> Result<DMat4> {
        if let Some(cached) = self.local_placement_cache.get(&id).copied() {
            return Ok(cached);
        }
        let (parent_ref, rel_ref) = {
            let entity = self.entity(id)?;
            (
                as_ref_id(entity.args.first()),
                as_ref_id(entity.args.get(1)),
            )
        };
        let entity = self.entity(id)?;
        if entity.entity_name != "IFCLOCALPLACEMENT" {
            anyhow::bail!(
                "#{id} is {}, expected IFCLOCALPLACEMENT",
                entity.entity_name
            );
        }
        let parent = match parent_ref {
            Some(parent_id) => self.local_placement_matrix(parent_id)?,
            None => identity_4(),
        };
        let relative = match rel_ref {
            Some(rel_id) => {
                let rel = self.entity(rel_id)?;
                match rel.entity_name.as_str() {
                    "IFCAXIS2PLACEMENT3D" => self.axis2placement3d_matrix(rel_id)?,
                    _ => identity_4(),
                }
            }
            None => identity_4(),
        };
        let m = parent * relative;
        self.local_placement_cache.insert(id, m);
        Ok(m)
    }

    fn profile_points_2d(&self, id: EntityId) -> Result<Vec<DVec2>> {
        let entity = self.entity(id)?;
        match entity.entity_name.as_str() {
            "IFCRECTANGLEPROFILEDEF" => {
                let xdim = as_real(entity.args.get(3))
                    .with_context(|| format!("IFCRECTANGLEPROFILEDEF #{id} missing xdim"))?;
                let ydim = as_real(entity.args.get(4))
                    .with_context(|| format!("IFCRECTANGLEPROFILEDEF #{id} missing ydim"))?;
                let mut points = vec![
                    DVec2::new(-xdim / 2.0, -ydim / 2.0),
                    DVec2::new(xdim / 2.0, -ydim / 2.0),
                    DVec2::new(xdim / 2.0, ydim / 2.0),
                    DVec2::new(-xdim / 2.0, ydim / 2.0),
                ];
                if let Some(pos_id) = as_ref_id(entity.args.get(2)) {
                    let (origin, x, y) = self.axis2placement2d(pos_id)?;
                    for p in &mut points {
                        *p = origin + x * p.x + y * p.y;
                    }
                }
                Ok(points)
            }
            "IFCARBITRARYCLOSEDPROFILEDEF" => {
                let curve_id = as_ref_id(entity.args.get(2))
                    .with_context(|| format!("IFCARBITRARYCLOSEDPROFILEDEF #{id} missing curve"))?;
                self.polyline_points_2d(curve_id)
            }
            "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                // args[0] = ProfileType (enum, ignore)
                // args[1] = ProfileName (string, ignore)
                // args[2] = OuterCurveRef
                // args[3] = list of inner void curve refs (ignored — outer profile only)
                let outer_curve_id = as_ref_id(entity.args.get(2)).with_context(|| {
                    format!("IFCARBITRARYPROFILEDEFWITHVOIDS #{id} missing outer curve")
                })?;
                self.polyline_points_2d(outer_curve_id)
            }
            other => anyhow::bail!("unsupported profile type {other} for entity #{id}"),
        }
    }

    fn polyline_points_2d(&self, id: EntityId) -> Result<Vec<DVec2>> {
        let entity = self.entity(id)?;
        match entity.entity_name.as_str() {
            "IFCPOLYLINE" => {
                let points = refs_from_list(entity.args.first());
                let mut out = Vec::with_capacity(points.len());
                for point_id in points {
                    out.push(self.cartesian_point_2d(point_id)?);
                }
                if out.len() >= 2 && out.first() == out.last() {
                    out.pop();
                }
                if out.len() < 3 {
                    anyhow::bail!("IFCPOLYLINE #{id} has fewer than 3 distinct points");
                }
                Ok(out)
            }
            "IFCINDEXEDPOLYCURVE" => self.indexed_polycurve_points_2d(id),
            other => anyhow::bail!("#{id} is {other}, expected IFCPOLYLINE or IFCINDEXEDPOLYCURVE"),
        }
    }

    fn indexed_polycurve_points_2d(&self, id: EntityId) -> Result<Vec<DVec2>> {
        let entity = self.entity(id)?;
        // IFCINDEXEDPOLYCURVE(PointsRef, Segments, SelfIntersect)
        // args[0] = ref to IfcCartesianPointList2D
        // args[1] = $ (no segments) OR list of IFCLINEINDEX/IFCARCINDEX typed values
        // args[2] = .T. or .F.
        let points_list_id = as_ref_id(entity.args.first())
            .with_context(|| format!("IFCINDEXEDPOLYCURVE #{id} missing PointsList ref"))?;
        let point_list_entity = self.entity(points_list_id)?;
        if point_list_entity.entity_name != "IFCCARTESIANPOINTLIST2D" {
            anyhow::bail!(
                "IFCINDEXEDPOLYCURVE #{id} points ref #{points_list_id} is {}, expected IFCCARTESIANPOINTLIST2D",
                point_list_entity.entity_name
            );
        }
        // Parse the flat coordinate list: args[0] = list of 2D pairs ((x1,y1),(x2,y2),...)
        let all_points: Vec<DVec2> = match point_list_entity.args.first() {
            Some(StepValue::List(pairs)) => {
                let mut pts = Vec::with_capacity(pairs.len());
                for pair in pairs {
                    match pair {
                        StepValue::List(coords) => {
                            let x = as_real(coords.first()).unwrap_or(0.0);
                            let y = as_real(coords.get(1)).unwrap_or(0.0);
                            pts.push(DVec2::new(x, y));
                        }
                        _ => anyhow::bail!(
                            "IFCCARTESIANPOINTLIST2D #{points_list_id} has non-list coordinate entry"
                        ),
                    }
                }
                pts
            }
            _ => anyhow::bail!("IFCCARTESIANPOINTLIST2D #{points_list_id} has invalid CoordList"),
        };

        let segments_arg = entity.args.get(1);
        let out = match segments_arg {
            None | Some(StepValue::Null) | Some(StepValue::Derived) => {
                // No explicit segments — use all points in order
                all_points
            }
            Some(StepValue::List(segments)) if segments.is_empty() => all_points,
            Some(StepValue::List(segments)) => {
                // Each segment is a typed value: IFCLINEINDEX(...) or IFCARCINDEX(...)
                let mut ordered_indices: Vec<usize> = Vec::new();
                for seg in segments {
                    match seg {
                        StepValue::Typed { type_name, value } => {
                            match type_name.as_str() {
                                "IFCLINEINDEX" => {
                                    // value is a List of 1-based integer indices
                                    let indices = match value.as_ref() {
                                        StepValue::List(idxs) => idxs,
                                        _ => anyhow::bail!(
                                            "IFCLINEINDEX in IFCINDEXEDPOLYCURVE #{id} has non-list value"
                                        ),
                                    };
                                    for idx_val in indices {
                                        let idx = match idx_val {
                                            StepValue::Int(i) => *i as usize,
                                            _ => anyhow::bail!(
                                                "IFCLINEINDEX in IFCINDEXEDPOLYCURVE #{id} has non-integer index"
                                            ),
                                        };
                                        if idx == 0 || idx > all_points.len() {
                                            anyhow::bail!(
                                                "IFCLINEINDEX index {idx} out of range for IFCCARTESIANPOINTLIST2D #{points_list_id} (len={})",
                                                all_points.len()
                                            );
                                        }
                                        // Only add if not a consecutive duplicate (avoids seam duplication between segments)
                                        if ordered_indices.last() != Some(&(idx - 1)) {
                                            ordered_indices.push(idx - 1);
                                        }
                                    }
                                }
                                "IFCARCINDEX" => {
                                    anyhow::bail!(
                                        "IFCARCINDEX segments are not supported in IFCINDEXEDPOLYCURVE #{id}"
                                    );
                                }
                                other => anyhow::bail!(
                                    "unsupported segment type {other} in IFCINDEXEDPOLYCURVE #{id}"
                                ),
                            }
                        }
                        _ => anyhow::bail!("non-typed segment value in IFCINDEXEDPOLYCURVE #{id}"),
                    }
                }
                ordered_indices.into_iter().map(|i| all_points[i]).collect()
            }
            _ => {
                // Treat as no segments — use all points in order
                all_points
            }
        };

        let mut out = out;
        // Drop closing duplicate if present
        if out.len() >= 2 && out.first() == out.last() {
            out.pop();
        }
        if out.len() < 3 {
            anyhow::bail!("IFCINDEXEDPOLYCURVE #{id} has fewer than 3 distinct points");
        }
        Ok(out)
    }

    fn cartesian_transformation_operator3d_matrix(&self, id: EntityId) -> Result<DMat4> {
        let entity = self.entity(id)?;
        if !entity
            .entity_name
            .starts_with("IFCCARTESIANTRANSFORMATIONOPERATOR3D")
        {
            anyhow::bail!(
                "#{id} is {}, expected IFCCARTESIANTRANSFORMATIONOPERATOR3D*",
                entity.entity_name
            );
        }
        let x = match as_ref_id(entity.args.first()) {
            Some(axis) => self.direction3(axis)?,
            None => DVec3::new(1.0, 0.0, 0.0),
        };
        let y = match as_ref_id(entity.args.get(1)) {
            Some(axis) => self.direction3(axis)?,
            None => DVec3::new(0.0, 1.0, 0.0),
        };
        let z = match as_ref_id(entity.args.get(4)) {
            Some(axis) => self.direction3(axis)?,
            None => normalize_or(x.cross(y), DVec3::new(0.0, 0.0, 1.0)),
        };
        let origin = self.cartesian_point(
            as_ref_id(entity.args.get(2))
                .with_context(|| format!("operator #{id} missing local origin"))?,
        )?;
        let s = as_real(entity.args.get(3)).unwrap_or(1.0);
        let sx = s;
        let sy = as_real(entity.args.get(5)).unwrap_or(s);
        let sz = as_real(entity.args.get(6)).unwrap_or(s);
        Ok(DMat4::from_cols(
            DVec4::new(x.x * sx, x.y * sx, x.z * sx, 0.0),
            DVec4::new(y.x * sy, y.y * sy, y.z * sy, 0.0),
            DVec4::new(z.x * sz, z.y * sz, z.z * sz, 0.0),
            DVec4::new(origin.x, origin.y, origin.z, 1.0),
        ))
    }

    fn build_representation_shape(&mut self, rep_id: EntityId, world: DMat4) -> Result<Shape> {
        let rep = self.entity(rep_id)?;
        if rep.entity_name != "IFCSHAPEREPRESENTATION" {
            anyhow::bail!(
                "#{rep_id} is {}, expected IFCSHAPEREPRESENTATION",
                rep.entity_name
            );
        }
        let item_ids = refs_from_list(rep.args.get(3));
        let mut shapes = Vec::new();
        for item_id in item_ids {
            if let Ok(shape) = self.build_representation_item_shape(item_id, world) {
                shapes.push(shape);
            }
        }
        combine_shapes(shapes)
            .with_context(|| format!("representation #{rep_id} had no buildable items"))
    }

    fn build_product_shape(&mut self, product_id: EntityId) -> Result<Shape> {
        let (placement_ref, pdef_ref) = {
            let product = self.entity(product_id)?;
            (
                as_ref_id(product.args.get(5)),
                as_ref_id(product.args.get(6)),
            )
        };
        let placement = match placement_ref {
            Some(id) => self.local_placement_matrix(id)?,
            None => identity_4(),
        };
        let pdef_id = pdef_ref
            .with_context(|| format!("product #{product_id} missing Representation_IfcProduct"))?;
        let pdef = self.entity(pdef_id)?;
        if pdef.entity_name != "IFCPRODUCTDEFINITIONSHAPE" {
            anyhow::bail!(
                "product #{product_id} representation #{pdef_id} is {}, expected IFCPRODUCTDEFINITIONSHAPE",
                pdef.entity_name
            );
        }
        let rep_ids = refs_from_list(pdef.args.get(2));
        let mut body_reps = Vec::new();
        for rep_id in &rep_ids {
            if let Ok(rep) = self.entity(*rep_id) {
                let is_body = rep
                    .args
                    .get(1)
                    .and_then(StepValue::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("Body"));
                if is_body {
                    body_reps.push(*rep_id);
                }
            }
        }
        let target_reps = if body_reps.is_empty() {
            rep_ids
        } else {
            body_reps
        };
        let mut shapes = Vec::new();
        for rep_id in target_reps {
            if let Ok(shape) = self.build_representation_shape(rep_id, placement) {
                shapes.push(shape);
            }
        }
        combine_shapes(shapes)
            .with_context(|| format!("product #{product_id} has no buildable body representation"))
    }

    fn build_representation_item_shape(
        &mut self,
        item_id: EntityId,
        world: DMat4,
    ) -> Result<Shape> {
        let item = self.entity(item_id)?;
        match item.entity_name.as_str() {
            "IFCEXTRUDEDAREASOLID" => self.build_extruded_area_solid(item_id, world),
            "IFCMAPPEDITEM" => self.build_mapped_item(item_id, world),
            "IFCBOOLEANCLIPPINGRESULT" => self.build_boolean_clipping_result(item_id, world),
            // Tessellated geometry: skip OCC building (produces shapes with 100s-1000s of faces
            // that make boolean O(n²) and painfully slow). Let these fall through to the
            // fallback_bboxes path which stores a simple box shape for bbox-based touching test.
            // A future voxel/octree approach would handle these precisely without OCC boolean.
            "IFCTRIANGULATEDFACESET" | "IFCPOLYGONALFACESET" => {
                anyhow::bail!("tessellated geometry #{item_id} deferred to bbox fallback")
            }
            other => anyhow::bail!("unsupported representation item {other} (#{item_id})"),
        }
    }

    fn build_triangulated_face_set(&mut self, item_id: EntityId, world: DMat4) -> Result<Shape> {
        let item = self.entity(item_id)?;
        // IfcTriangulatedFaceSet: args[0]=Coordinates(IfcCartesianPointList3D), args[1]=Normals($), args[2]=Closed, args[3]=CoordIndex
        let coords_id = as_ref_id(item.args.first())
            .with_context(|| format!("IFCTRIANGULATEDFACESET #{item_id} missing Coordinates"))?;
        let coords_entity = self.entity(coords_id)?;
        // IfcCartesianPointList3D: args[0] = list of [x,y,z] lists
        let coord_list = match coords_entity.args.first() {
            Some(StepValue::List(list)) => list.clone(),
            _ => anyhow::bail!("IFCCARTESIANPOINTLIST3D #{coords_id} missing CoordList"),
        };
        let mut vertices: Vec<f64> = Vec::with_capacity(coord_list.len() * 3);
        for coord_val in &coord_list {
            let StepValue::List(xyz) = coord_val else {
                anyhow::bail!("IFCCARTESIANPOINTLIST3D #{coords_id} coordinate is not a list");
            };
            let x =
                as_real(xyz.get(0)).with_context(|| format!("coord x missing in #{coords_id}"))?;
            let y =
                as_real(xyz.get(1)).with_context(|| format!("coord y missing in #{coords_id}"))?;
            let z =
                as_real(xyz.get(2)).with_context(|| format!("coord z missing in #{coords_id}"))?;
            let p = transform_point(world, DVec3::new(x, y, z));
            vertices.push(p.x);
            vertices.push(p.y);
            vertices.push(p.z);
        }
        // CoordIndex: args[3] = list of [i,j,k] 1-based index triples
        let index_list = match item.args.get(3) {
            Some(StepValue::List(list)) => list.clone(),
            _ => anyhow::bail!("IFCTRIANGULATEDFACESET #{item_id} missing CoordIndex"),
        };
        let mut indices: Vec<i32> = Vec::with_capacity(index_list.len() * 3);
        for tri_val in &index_list {
            let StepValue::List(ijk) = tri_val else {
                anyhow::bail!("IFCTRIANGULATEDFACESET #{item_id} CoordIndex entry is not a list");
            };
            let a = as_int(ijk.get(0))
                .with_context(|| format!("tri index a missing in #{item_id}"))?
                - 1;
            let b = as_int(ijk.get(1))
                .with_context(|| format!("tri index b missing in #{item_id}"))?
                - 1;
            let c = as_int(ijk.get(2))
                .with_context(|| format!("tri index c missing in #{item_id}"))?
                - 1;
            indices.push(a as i32);
            indices.push(b as i32);
            indices.push(c as i32);
        }
        Shape::from_triangle_mesh(&vertices, &indices)
            .with_context(|| format!("IFCTRIANGULATEDFACESET #{item_id} OCC sewing failed"))
    }

    fn build_polygonal_face_set(&mut self, item_id: EntityId, world: DMat4) -> Result<Shape> {
        let item = self.entity(item_id)?;
        // IfcPolygonalFaceSet: args[0]=Coordinates(IfcCartesianPointList3D), args[1]=Closed, args[2]=Faces
        let coords_id = as_ref_id(item.args.first())
            .with_context(|| format!("IFCPOLYGONALFACESET #{item_id} missing Coordinates"))?;
        let coords_entity = self.entity(coords_id)?;
        let coord_list = match coords_entity.args.first() {
            Some(StepValue::List(list)) => list.clone(),
            _ => anyhow::bail!("IFCCARTESIANPOINTLIST3D #{coords_id} missing CoordList"),
        };
        let mut world_points: Vec<DVec3> = Vec::with_capacity(coord_list.len());
        for coord_val in &coord_list {
            let StepValue::List(xyz) = coord_val else {
                anyhow::bail!("coord not a list in #{coords_id}");
            };
            let x =
                as_real(xyz.get(0)).with_context(|| format!("coord x missing in #{coords_id}"))?;
            let y =
                as_real(xyz.get(1)).with_context(|| format!("coord y missing in #{coords_id}"))?;
            let z =
                as_real(xyz.get(2)).with_context(|| format!("coord z missing in #{coords_id}"))?;
            world_points.push(transform_point(world, DVec3::new(x, y, z)));
        }
        // Faces: args[2] = list of face refs (IfcIndexedPolygonalFace or IfcIndexedPolygonalFaceWithVoids)
        let face_id_list = refs_from_list(item.args.get(2));
        // Triangulate each polygonal face (fan triangulation from first vertex)
        let mut vertices: Vec<f64> = Vec::new();
        let mut indices: Vec<i32> = Vec::new();
        for face_id in face_id_list {
            let face_entity = self.entity(face_id)?;
            // IfcIndexedPolygonalFace: args[0] = list of 1-based indices (CoordIndex)
            let idx_list = match face_entity.args.first() {
                Some(StepValue::List(list)) => list.clone(),
                _ => continue,
            };
            let face_indices: Vec<usize> = idx_list
                .iter()
                .filter_map(|v| as_int(Some(v)).ok().map(|i| (i - 1) as usize))
                .collect();
            if face_indices.len() < 3 {
                continue;
            }
            // Fan triangulate: (0,1,2), (0,2,3), (0,3,4), ...
            let base = vertices.len() / 3;
            for &fi in &face_indices {
                if fi >= world_points.len() {
                    continue;
                }
                let p = world_points[fi];
                vertices.push(p.x);
                vertices.push(p.y);
                vertices.push(p.z);
            }
            for tri in 1..(face_indices.len() - 1) {
                indices.push(base as i32);
                indices.push((base + tri) as i32);
                indices.push((base + tri + 1) as i32);
            }
        }
        Shape::from_triangle_mesh(&vertices, &indices)
            .with_context(|| format!("IFCPOLYGONALFACESET #{item_id} OCC sewing failed"))
    }

    fn build_extruded_area_solid(&mut self, solid_id: EntityId, world: DMat4) -> Result<Shape> {
        let solid = self.entity(solid_id)?;
        let profile_id = as_ref_id(solid.args.first())
            .with_context(|| format!("IFCEXTRUDEDAREASOLID #{solid_id} missing profile"))?;
        let profile_pts = self.profile_points_2d(profile_id)?;
        let swept_matrix = match as_ref_id(solid.args.get(1)) {
            Some(pos_id) => self.axis2placement3d_matrix(pos_id)?,
            None => identity_4(),
        };
        let base = world * swept_matrix;
        let dir_local = self
            .direction3(as_ref_id(solid.args.get(2)).with_context(|| {
                format!("IFCEXTRUDEDAREASOLID #{solid_id} missing direction")
            })?)?;
        let depth = as_real(solid.args.get(3))
            .with_context(|| format!("IFCEXTRUDEDAREASOLID #{solid_id} missing depth"))?;
        let dir = transform_vector(base, dir_local * depth);
        if dir.length() <= 1e-12 {
            anyhow::bail!("IFCEXTRUDEDAREASOLID #{solid_id} produced zero extrusion vector");
        }
        let world_points: Vec<DVec3> = profile_pts
            .into_iter()
            .map(|p| transform_point(base, DVec3::new(p.x, p.y, 0.0)))
            .collect();
        let face = Face::from_polygon(&world_points).map_err(|error| {
            anyhow::anyhow!("failed to build profile face for #{solid_id}: {error}")
        })?;
        let solid = face
            .extrude(dir)
            .map_err(|error| anyhow::anyhow!("failed to extrude #{solid_id}: {error}"))?;
        Ok(solid.into())
    }

    fn build_mapped_item(&mut self, mapped_id: EntityId, world: DMat4) -> Result<Shape> {
        let mapped = self.entity(mapped_id)?;
        let map_source_id = as_ref_id(mapped.args.first())
            .with_context(|| format!("IFCMAPPEDITEM #{mapped_id} missing MappingSource"))?;
        let target_id = as_ref_id(mapped.args.get(1))
            .with_context(|| format!("IFCMAPPEDITEM #{mapped_id} missing MappingTarget"))?;
        let map_source = self.entity(map_source_id)?;
        if map_source.entity_name != "IFCREPRESENTATIONMAP" {
            anyhow::bail!(
                "mapped source #{map_source_id} is {}, expected IFCREPRESENTATIONMAP",
                map_source.entity_name
            );
        }
        let map_origin = match as_ref_id(map_source.args.first()) {
            Some(origin_id) => self.axis2placement3d_matrix(origin_id)?,
            None => identity_4(),
        };
        let mapped_rep = as_ref_id(map_source.args.get(1)).with_context(|| {
            format!("IFCREPRESENTATIONMAP #{map_source_id} missing mapped representation")
        })?;
        let target = self.cartesian_transformation_operator3d_matrix(target_id)?;
        let mapped_world = world * target * map_origin.inverse();
        self.build_representation_shape(mapped_rep, mapped_world)
    }

    fn plane_origin_normal(&self, plane_id: EntityId, world: DMat4) -> Result<(DVec3, DVec3)> {
        let plane = self.entity(plane_id)?;
        if plane.entity_name != "IFCPLANE" {
            anyhow::bail!("#{plane_id} is {}, expected IFCPLANE", plane.entity_name);
        }
        let place_id = as_ref_id(plane.args.first())
            .with_context(|| format!("IFCPLANE #{plane_id} missing position"))?;
        let m = world * self.axis2placement3d_matrix(place_id)?;
        let origin = transform_point(m, DVec3::new(0.0, 0.0, 0.0));
        let normal = normalize_or(
            transform_vector(m, DVec3::new(0.0, 0.0, 1.0)),
            DVec3::new(0.0, 0.0, 1.0),
        );
        Ok((origin, normal))
    }

    fn build_halfspace_shape(&self, halfspace_id: EntityId, world: DMat4) -> Result<Shape> {
        let halfspace = self.entity(halfspace_id)?;
        match halfspace.entity_name.as_str() {
            "IFCPOLYGONALBOUNDEDHALFSPACE" => {
                let plane_id = as_ref_id(halfspace.args.first()).with_context(|| {
                    format!("IFCPOLYGONALBOUNDEDHALFSPACE #{halfspace_id} missing base plane")
                })?;
                let agreement = as_bool(halfspace.args.get(1)).unwrap_or(true);
                let (origin, normal) = self.plane_origin_normal(plane_id, world)?;
                let n = if agreement { normal } else { -normal };
                Ok(Shape::half_space(origin, n))
            }
            "IFCHALFSPACESOLID" => {
                let plane_id = as_ref_id(halfspace.args.first()).with_context(|| {
                    format!("IFCHALFSPACESOLID #{halfspace_id} missing base plane")
                })?;
                let agreement = as_bool(halfspace.args.get(1)).unwrap_or(true);
                let (origin, normal) = self.plane_origin_normal(plane_id, world)?;
                let n = if agreement { normal } else { -normal };
                Ok(Shape::half_space(origin, n))
            }
            other => anyhow::bail!("unsupported halfspace operand {other} (#{halfspace_id})"),
        }
    }

    fn build_boolean_clipping_result(&mut self, clip_id: EntityId, world: DMat4) -> Result<Shape> {
        let (op, first_id, second_id) = {
            let clip = self.entity(clip_id)?;
            let op = clip
                .args
                .first()
                .and_then(StepValue::as_enum)
                .unwrap_or("DIFFERENCE")
                .to_string();
            let first_id = as_ref_id(clip.args.get(1)).with_context(|| {
                format!("IFCBOOLEANCLIPPINGRESULT #{clip_id} missing first operand")
            })?;
            let second_id = as_ref_id(clip.args.get(2)).with_context(|| {
                format!("IFCBOOLEANCLIPPINGRESULT #{clip_id} missing second operand")
            })?;
            (op, first_id, second_id)
        };
        let first = self.build_representation_item_shape(first_id, world)?;
        let second_entity = self.entity(second_id)?;
        let second = if second_entity.entity_name.ends_with("HALFSPACE")
            || second_entity.entity_name == "IFCPOLYGONALBOUNDEDHALFSPACE"
        {
            self.build_halfspace_shape(second_id, world)?
        } else {
            self.build_representation_item_shape(second_id, world)?
        };
        let result = match op.as_str() {
            "DIFFERENCE" => {
                first
                    .subtract(&second)
                    .map_err(|error| {
                        anyhow::anyhow!("boolean difference failed for #{clip_id}: {error}")
                    })?
                    .shape
            }
            "INTERSECTION" => {
                first
                    .intersect(&second)
                    .map_err(|error| {
                        anyhow::anyhow!("boolean intersection failed for #{clip_id}: {error}")
                    })?
                    .shape
            }
            "UNION" => {
                first
                    .union(&second)
                    .map_err(|error| {
                        anyhow::anyhow!("boolean union failed for #{clip_id}: {error}")
                    })?
                    .shape
            }
            _ => anyhow::bail!("unsupported clipping operator {op} for #{clip_id}"),
        };
        Ok(result)
    }
}

fn combine_shapes(mut shapes: Vec<Shape>) -> Result<Shape> {
    let Some(mut acc) = shapes.pop() else {
        anyhow::bail!("no shapes to combine");
    };
    for shape in shapes {
        acc = acc
            .union(&shape)
            .map_err(|error| anyhow::anyhow!("shape union failed: {error}"))?
            .shape;
    }
    Ok(acc)
}

fn seed_cache_from_ifc(
    cache_dir: &Path,
    ifc_path: &Path,
    required_ids: &HashSet<EntityId>,
) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("failed creating cache dir {}", cache_dir.display()))?;
    let step = parse_step_file(ifc_path)
        .with_context(|| format!("failed to parse IFC {}", ifc_path.display()))?;
    let mut builder = IfcGeometryBuilder::new(&step.entities);
    for entity_id in required_ids {
        let path = shape_path(cache_dir, *entity_id);
        if path.exists() {
            continue;
        }
        let Ok(shape) = builder.build_product_shape(*entity_id) else {
            continue;
        };
        let mut file = File::create(&path)
            .with_context(|| format!("failed creating BRep file {}", path.display()))?;
        shape
            .write_brep_bin(&mut file)
            .map_err(|error| anyhow::anyhow!("failed writing BRep {}: {error}", path.display()))?;
    }
    Ok(())
}

fn build_shapes_in_memory_from_ifc(
    ifc_path: &Path,
    required_ids: &HashSet<EntityId>,
    fallback_bboxes: &HashMap<u64, Vec<f64>>,
) -> Result<HashMap<EntityId, (Shape, bool)>> {
    let step = parse_step_file(ifc_path)
        .with_context(|| format!("failed to parse IFC {}", ifc_path.display()))?;
    let mut builder = IfcGeometryBuilder::new(&step.entities);
    let mut shapes: HashMap<EntityId, (Shape, bool)> = HashMap::with_capacity(required_ids.len());

    for &entity_id in required_ids {
        if let Ok(shape) = builder.build_product_shape(entity_id) {
            shapes.insert(entity_id, (shape, true));
        }
    }

    let mut missing: Vec<EntityId> = required_ids
        .iter()
        .copied()
        .filter(|id| !shapes.contains_key(id))
        .collect();
    if missing.is_empty() {
        return Ok(shapes);
    }

    let mut from_fallback_bbox = 0usize;
    let mut still_need_ifcopenshell = Vec::new();
    for &id in &missing {
        if let Some(bbox_vec) = fallback_bboxes.get(&id) {
            if bbox_vec.len() == 6 {
                let box_shape = Shape::box_from_corners(
                    glam::DVec3::new(bbox_vec[0], bbox_vec[1], bbox_vec[2]),
                    glam::DVec3::new(bbox_vec[3], bbox_vec[4], bbox_vec[5]),
                );
                shapes.insert(id, (box_shape, false));
                from_fallback_bbox += 1;
                continue;
            }
        }
        still_need_ifcopenshell.push(id);
    }
    if from_fallback_bbox > 0 {
        eprintln!(
            "Used CLI fallback bboxes for {} entities",
            from_fallback_bbox
        );
    }

    if !still_need_ifcopenshell.is_empty() {
        eprintln!(
            "OCC failed for {} entities with no fallback bbox — trying IfcOpenShell fallback",
            still_need_ifcopenshell.len()
        );
        let fallback_shapes = build_shapes_via_ifcopenshell(ifc_path, &still_need_ifcopenshell);
        eprintln!(
            "IfcOpenShell fallback recovered {} / {} entities",
            fallback_shapes.len(),
            still_need_ifcopenshell.len()
        );
        for (entity_id, shape) in fallback_shapes {
            if let Some(bbox) = shape.bounding_box() {
                let box_shape = Shape::box_from_corners(
                    glam::DVec3::new(bbox[0], bbox[1], bbox[2]),
                    glam::DVec3::new(bbox[3], bbox[4], bbox[5]),
                );
                shapes.insert(entity_id, (box_shape, false));
            }
        }
    }

    missing.clear();
    Ok(shapes)
}

fn prebuild_cache_from_ifc(cache_dir: &Path, ifc_path: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("failed creating cache dir {}", cache_dir.display()))?;
    let step = parse_step_file(ifc_path)
        .with_context(|| format!("failed to parse IFC {}", ifc_path.display()))?;
    let mut builder = IfcGeometryBuilder::new(&step.entities);
    let mut ids: Vec<EntityId> = step
        .entities
        .values()
        .filter_map(|entity| as_ref_id(entity.args.get(6)).map(|_| entity.id))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    // Phase 1: build all shapes we can via our OCC geometry builder.
    let mut occ_failed: Vec<EntityId> = Vec::new();
    for &entity_id in &ids {
        let path = shape_path(cache_dir, entity_id);
        if path.exists() {
            continue;
        }
        match builder.build_product_shape(entity_id) {
            Ok(shape) => {
                let mut file = File::create(&path)
                    .with_context(|| format!("failed creating BRep file {}", path.display()))?;
                shape
                    .write_brep_bin(&mut file)
                    .map_err(|e| anyhow::anyhow!("failed writing BRep {}: {e}", path.display()))?;
            }
            Err(_) => occ_failed.push(entity_id),
        }
    }

    // Phase 2: for entities that OCC couldn't handle (tessellated, etc.), fall back to
    // IfcOpenShell via PyO3. IfcOpenShell handles all IFC geometry types natively.
    if !occ_failed.is_empty() {
        eprintln!(
            "OCC failed for {} entities — trying IfcOpenShell fallback",
            occ_failed.len()
        );
        let shapes = build_shapes_via_ifcopenshell(ifc_path, &occ_failed);
        let n_recovered = shapes.len();
        for (entity_id, shape) in shapes {
            let path = shape_path(cache_dir, entity_id);
            let mut file = File::create(&path)
                .with_context(|| format!("failed creating BRep file {}", path.display()))?;
            shape
                .write_brep_bin(&mut file)
                .map_err(|e| anyhow::anyhow!("failed writing BRep {}: {e}", path.display()))?;
        }
        eprintln!(
            "IfcOpenShell recovered {n_recovered} / {} entities",
            occ_failed.len()
        );
    }

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(ifc_path) = args.prebuild_cache_from_ifc.as_deref() {
        let cache_dir = args
            .brep_cache_dir
            .clone()
            .unwrap_or_else(|| default_cache_dir(&ifc_path.to_string_lossy()));
        prebuild_cache_from_ifc(&cache_dir, ifc_path)?;
        return Ok(());
    }

    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("failed reading kernel batch request from stdin")?;

    let request: BatchRequest =
        serde_json::from_str(&input).context("failed parsing kernel batch request JSON")?;
    let ifc_path = Path::new(&request.ifc_path);

    // Step 1: Collect all unique entity IDs needed for the received pairs.
    let needed_ids: HashSet<EntityId> = request
        .pairs
        .iter()
        .flat_map(|p| [p.left, p.right])
        .collect();

    // Step 2: Build shapes in memory for all needed IDs.
    let shapes = build_shapes_in_memory_from_ifc(ifc_path, &needed_ids, &request.fallback_bboxes)?;

    // Step 3: Run exact intersection test for every pair in parallel.
    // Pairs where either shape could not be built return no-intersect with no error.
    let results: Vec<BatchPairResponse> = request
        .pairs
        .iter()
        .map(|pair| analyze_pair_in_memory(&shapes, pair.left, pair.right, request.tolerance))
        .collect();

    let response = BatchResponse { results };
    serde_json::to_writer(io::stdout(), &response)
        .context("failed writing kernel response JSON to stdout")?;
    io::stdout()
        .write_all(b"\n")
        .context("failed writing response newline")?;
    Ok(())
}
