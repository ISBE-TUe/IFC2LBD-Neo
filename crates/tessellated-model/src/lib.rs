//! Shared data model for tessellated IFC geometry.
//!
//! Stored as `Arc<TessellatedModel>` in `PipelineContext`. Read by
//! `plugin-geometry-producer` and any future geometry-consuming module
//! (topology, QTO, clash, etc.).

pub use ifc_geometry::{FlatMesh, GeometryInstance, Mesh};

/// The complete tessellated geometry of one IFC model, ready for export.
#[derive(Debug, Clone)]
pub struct TessellatedModel {
    pub meshes: Vec<FlatMesh>,
    /// Column-major 4×4 coordination matrix (from IFCCOORDINATEOPERATION or identity)
    pub coordination_matrix: [f64; 16],
    pub metadata_mode: MetadataMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetadataMode {
    /// Include GUIDs, categories, attributes and relations (default).
    #[default]
    Full,
    /// Geometry + GUIDs only — for visualization-only workflows.
    Stripped,
}

impl TessellatedModel {
    pub fn new(meshes: Vec<FlatMesh>, metadata_mode: MetadataMode) -> Self {
        Self {
            meshes,
            coordination_matrix: IDENTITY_4X4,
            metadata_mode,
        }
    }
}

const IDENTITY_4X4: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];
