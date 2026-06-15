//! Geometry producer plugin.
//!
//! Reads TessellatedModel from context, serializes to chosen format, emits
//! via sidecar_tx. Supported formats: fragments (default), gltf, parquet, ifc5.


use crossbeam::channel::Sender;
use lbd_pipeline::{
    DerivedFile, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};
use tessellated_model::TessellatedModel;

pub const GEOMETRY_PRODUCER_ID: &str = "neo-geometry-producer";

/// Multiply two column-major 4×4 matrices: C = A * B.
///
/// Used by glTF and Parquet writers to combine world_transform and local_transform
/// before baking into vertex positions. Fragments skips this because its viewer
/// applies both transforms at runtime.
pub(crate) fn mul_mat4(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut c = [0.0f64; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 { s += a[k * 4 + row] * b[col * 4 + k]; }
            c[col * 4 + row] = s;
        }
    }
    c
}

/// Content-signature hash of a position-free ifc-lite triangulated mesh, for shell dedup.
///
/// Mirrors `fragments_core::hash_shell` but operates on ifc-lite's deterministic
/// triangulation instead of the oracle polygon mesh. Because ifc-lite produces the
/// same triangle count and the same invariants (vertex/triangle counts, summed
/// triangle area, signed volume, centroid) for the same shape regardless of where
/// it sits in the model, two STEP entities with identical geometry hash identically
/// — closing the dedup gap that per-STEP-entity-ID dedup leaves open.
///
/// Requires the mesh to be in position-free (definition) space, which the geometry
/// un-bake guarantees; otherwise placement would leak into the centroid term.
#[cfg(feature = "fmt-fragments")]
/// Column-major 4×4 pure translation matrix.
pub(crate) fn translate_colmajor(t: [f64; 3]) -> [f64; 16] {
    [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        t[0], t[1], t[2], 1.0,
    ]
}

/// Min-corner (axis-aligned bounding-box minimum) of a mesh, in f64.
/// Used as the translation-normalization origin for shell dedup.
pub(crate) fn mesh_min_corner(mesh: &ifc_geometry::Mesh) -> [f64; 3] {
    let mut min = [f64::MAX; 3];
    for c in mesh.positions.chunks_exact(3) {
        min[0] = min[0].min(c[0] as f64);
        min[1] = min[1].min(c[1] as f64);
        min[2] = min[2].min(c[2] as f64);
    }
    if min[0] == f64::MAX { return [0.0; 3]; }
    min
}

/// Exact content hash of a mesh: quantized vertices (relative to `off`) + index list.
///
/// Passing the mesh min-corner as `off` makes the hash TRANSLATION-INVARIANT, so
/// identical shapes at different positions (repeated railing balusters, explicit
/// IfcTriangulatedFaceSets with placement baked into vertices) collapse to one shell;
/// the offset is re-applied per instance via the sample transform.
///
/// Hashing the actual vertex/index data (quantized to 0.1 mm) rather than a lossy
/// area/volume signature is critical for CORRECTNESS: a signature hash collides on
/// distinct shells that merely share invariants (e.g. a shape and its rotated/mirrored
/// twin), which merges unrelated geometry and renders one element with another's shell.
pub(crate) fn hash_ifc_mesh(mesh: &ifc_geometry::Mesh, off: [f64; 3]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    (mesh.positions.len() / 3).hash(&mut h);
    mesh.indices.len().hash(&mut h);

    // Quantize to 0.1 mm to absorb fp noise from independent-but-identical tessellations.
    let q = |v: f32, o: f64| -> i64 { ((v as f64 - o) * 10000.0).round() as i64 };
    for c in mesh.positions.chunks_exact(3) {
        q(c[0], off[0]).hash(&mut h);
        q(c[1], off[1]).hash(&mut h);
        q(c[2], off[2]).hash(&mut h);
    }
    for idx in &mesh.indices {
        idx.hash(&mut h);
    }
    h.finish()
}

/// IFC uses a Z-up right-handed coordinate system; glTF expects Y-up right-handed.
/// Conversion: x' = x, y' = z, z' = -y  (column-major, applied left of world*local).
pub(crate) const IFC_TO_GLTF: [f64; 16] = [
    1.0, 0.0,  0.0, 0.0,   // col 0
    0.0, 0.0, -1.0, 0.0,   // col 1
    0.0, 1.0,  0.0, 0.0,   // col 2
    0.0, 0.0,  0.0, 1.0,   // col 3
];

/// Runtime config stored in PipelineContext by the CLI/WASM runner.
#[derive(Clone)]
pub struct GeometryProducerConfig {
    pub format: GeometryFormat,
    /// LBD base namespace (already normalized, no trailing slash). Used to stamp
    /// the element/spatial resource IRI onto each 3D object so the viewer links a
    /// clicked object straight to its LBD node — same IRI the LBD converter emits.
    pub base_uri: String,
}

/// Output format for the geometry producer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GeometryFormat {
    #[default]
    Fragments,
    Gltf,
    Parquet,
    Ifc5,
}

impl GeometryFormat {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fragments" => Some(Self::Fragments),
            "gltf" => Some(Self::Gltf),
            "parquet" => Some(Self::Parquet),
            "ifc5" => Some(Self::Ifc5),
            _ => None,
        }
    }

    pub fn filename(&self) -> &'static str {
        match self {
            Self::Fragments => "model.frag",
            Self::Gltf => "model.glb",
            Self::Parquet => "model.parquet",
            Self::Ifc5 => "model.ifcx",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Fragments => "application/octet-stream",
            Self::Gltf => "model/gltf-binary",
            Self::Parquet => "application/x-parquet",
            Self::Ifc5 => "application/json",
        }
    }
}

pub struct GeometryProducerPlugin {
    pub format: GeometryFormat,
}

impl GeometryProducerPlugin {
    pub fn new(format: GeometryFormat) -> Self {
        Self { format }
    }
}

impl Default for GeometryProducerPlugin {
    fn default() -> Self {
        Self { format: GeometryFormat::Fragments }
    }
}

impl PipelinePlugin for GeometryProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: GEOMETRY_PRODUCER_ID,
            display_name: "Geometry producer",
            stage: PipelineStage::Produce,
            description: "Serializes tessellated geometry to fragments, glTF, Parquet or IFC5.",
            inputs: vec!["tessellated-model"],
            outputs: vec!["geometry-sidecar"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for GeometryProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        _sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<TessellatedModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "GeometryProducerPlugin: missing TessellatedModel in context. \
                 Enable neo-geometry-preprocess before this producer."
                    .to_string(),
            )
        })?;

        // Format from context config overrides plugin default
        let format = ctx.get::<GeometryProducerConfig>()
            .map(|c| c.format)
            .unwrap_or(self.format);

        let bytes = match format {
            GeometryFormat::Fragments => serialize_fragments(ctx, &model)?,
            GeometryFormat::Gltf => serialize_gltf(ctx, &model)?,
            GeometryFormat::Parquet => serialize_parquet(ctx, &model)?,
            GeometryFormat::Ifc5 => {
                return Err(ProducerError::Conversion(
                    "IFC5 format not yet implemented".to_string(),
                ))
            }
        };

        if let Some(tx) = &ctx.sidecar_tx {
            let _ = tx.send(DerivedFile {
                filename: format.filename().to_string(),
                mime_type: format.mime_type(),
                bytes,
            });
        }

        Ok(())
    }
}

/// LBD base namespace for stamping resource IRIs onto 3D objects. Already
/// normalized (no trailing slash) by the CLI/WASM runner. Falls back to the
/// LBD default if the config is absent so IRIs still match the converter default.
fn geometry_base_uri(ctx: &PipelineContext) -> String {
    ctx.get::<GeometryProducerConfig>()
        .map(|c| c.base_uri.clone())
        .unwrap_or_else(|| "https://lbd.example.com".to_string())
}

// ─── Fragments serialization ──────────────────────────────────────────────────

#[cfg(feature = "fmt-fragments")]
fn serialize_fragments(
    ctx: &PipelineContext,
    model: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    use ifc_model::IfcModel;
    use ifc_step::StepFile;

    let ifc_model = ctx.get::<IfcModel>().ok_or_else(|| {
        ProducerError::Conversion("GeometryProducerPlugin: missing IfcModel".to_string())
    })?;
    let step = ctx.get::<StepFile>().ok_or_else(|| {
        ProducerError::Conversion("GeometryProducerPlugin: missing StepFile".to_string())
    })?;
    let base = geometry_base_uri(ctx);

    fragments::build_fragments(model, &ifc_model, &step, &base)
        .map_err(|e| ProducerError::Conversion(format!("fragments build failed: {e}")))
}

#[cfg(not(feature = "fmt-fragments"))]
fn serialize_fragments(
    _ctx: &PipelineContext,
    _model: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    Err(ProducerError::Conversion(
        "fragments format not compiled in (enable feature fmt-fragments)".to_string(),
    ))
}

// ─── Parquet serialization ────────────────────────────────────────────────────

// ─── glTF binary (GLB) serialization ─────────────────────────────────────────

#[cfg(feature = "fmt-gltf")]
fn serialize_gltf(
    ctx: &PipelineContext,
    tessellated: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    use ifc_model::IfcModel;
    let ifc_model = ctx.get::<IfcModel>();
    let base = geometry_base_uri(ctx);
    gltf_writer::write(tessellated, ifc_model.as_deref(), &base)
        .map_err(|e| ProducerError::Conversion(format!("glTF: {e}")))
}

#[cfg(not(feature = "fmt-gltf"))]
fn serialize_gltf(
    _ctx: &PipelineContext,
    _model: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    Err(ProducerError::Conversion("glTF not compiled (enable fmt-gltf feature)".into()))
}

/// glTF 2.0 binary (GLB) writer.
///
/// Spec: https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html
/// One mesh per element, world-space positions + normals + indices.
#[cfg(feature = "fmt-gltf")]
mod gltf_writer {
    use super::*;
    use serde_json::json;

    /// LBD resource IRI for a tessellated object, looked up by express id. Spatial
    /// nodes and elements get their canonical IRI; anything else returns None so
    /// the caller can fall back to the raw GUID.
    fn object_resource_iri(
        model: Option<&ifc_model::IfcModel>,
        base: &str,
        express_id: u64,
    ) -> Option<String> {
        let model = model?;
        if let Some(node) = model.spatial_nodes.get(&express_id) {
            return Some(ifc_model::iri::spatial_node_resource_iri(base, node));
        }
        if let Some(node) = model.elements.get(&express_id) {
            return Some(ifc_model::iri::element_resource_iri(base, node));
        }
        None
    }

    pub fn write(
        tessellated: &TessellatedModel,
        model: Option<&ifc_model::IfcModel>,
        base: &str,
    ) -> Result<Vec<u8>, String> {
        let mut bin: Vec<u8> = Vec::new();

        // Accessor indices and metadata for building the mesh JSON section
        struct MeshInfo {
            pos_bv: usize,
            nrm_bv: usize,
            idx_bv: usize,
            name: String,
            material_idx: usize,
        }

        // Material dedup: [r,g,b,a u8] → index
        let mut mat_map: std::collections::HashMap<[u8; 4], usize> = std::collections::HashMap::new();
        let mut materials_json: Vec<serde_json::Value> = Vec::new();

        let mut meshes_info: Vec<MeshInfo> = Vec::new();
        let mut buffer_views_json: Vec<serde_json::Value> = Vec::new();
        let mut accessors_json: Vec<serde_json::Value> = Vec::new();

        for flat in &tessellated.meshes {
            if flat.geometries.is_empty() { continue; }

            let mut all_pos: Vec<f32> = Vec::new();
            let mut all_nrm: Vec<f32> = Vec::new();
            let mut all_idx: Vec<u32> = Vec::new();
            let mut base_v: u32 = 0;
            let first_color = flat.geometries[0].color;

            // Use ifc-lite transforms: translations are in meters (scale_transform applied),
            // vertices are also in meters (scale_mesh applied) — consistent units throughout.
            for geom in &flat.geometries {
                let combined = mul_mat4(&IFC_TO_GLTF, &mul_mat4(&geom.world_transform, &geom.local_transform));
                bake_geom(&geom.mesh, &combined, &mut all_pos, &mut all_nrm, &mut all_idx, &mut base_v);
            }

            if all_idx.is_empty() { continue; }

            // Material index
            let color_bytes = [
                (first_color[0] * 255.0) as u8,
                (first_color[1] * 255.0) as u8,
                (first_color[2] * 255.0) as u8,
                (first_color[3] * 255.0) as u8,
            ];
            let mat_idx = *mat_map.entry(color_bytes).or_insert_with(|| {
                let idx = materials_json.len();
                let r = color_bytes[0] as f64 / 255.0;
                let g = color_bytes[1] as f64 / 255.0;
                let b = color_bytes[2] as f64 / 255.0;
                let a = color_bytes[3] as f64 / 255.0;
                materials_json.push(json!({
                    "pbrMetallicRoughness": {
                        "baseColorFactor": [r, g, b, a],
                        "metallicFactor": 0.0,
                        "roughnessFactor": 0.8
                    },
                    "alphaMode": if a < 1.0 { "BLEND" } else { "OPAQUE" }
                }));
                idx
            });

            // Compute bounding box
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            for chunk in all_pos.chunks_exact(3) {
                for a in 0..3 {
                    min[a] = min[a].min(chunk[a]);
                    max[a] = max[a].max(chunk[a]);
                }
            }

            // Positions buffer view + accessor
            let pos_bv_idx = buffer_views_json.len();
            let pos_byte_offset = bin.len();
            for v in &all_pos { bin.extend_from_slice(&v.to_le_bytes()); }
            // 4-byte align
            while bin.len() % 4 != 0 { bin.push(0); }
            let pos_byte_len = bin.len() - pos_byte_offset;
            buffer_views_json.push(json!({
                "buffer": 0,
                "byteOffset": pos_byte_offset,
                "byteLength": pos_byte_len,
                "target": 34962  // ARRAY_BUFFER
            }));
            let pos_acc_idx = accessors_json.len();
            accessors_json.push(json!({
                "bufferView": pos_bv_idx,
                "byteOffset": 0,
                "componentType": 5126,  // FLOAT
                "count": all_pos.len() / 3,
                "type": "VEC3",
                "min": [min[0], min[1], min[2]],
                "max": [max[0], max[1], max[2]]
            }));

            // Normals buffer view + accessor
            let nrm_bv_idx = buffer_views_json.len();
            let nrm_byte_offset = bin.len();
            for v in &all_nrm { bin.extend_from_slice(&v.to_le_bytes()); }
            while bin.len() % 4 != 0 { bin.push(0); }
            let nrm_byte_len = bin.len() - nrm_byte_offset;
            buffer_views_json.push(json!({
                "buffer": 0,
                "byteOffset": nrm_byte_offset,
                "byteLength": nrm_byte_len,
                "target": 34962
            }));
            let nrm_acc_idx = accessors_json.len();
            accessors_json.push(json!({
                "bufferView": nrm_bv_idx,
                "byteOffset": 0,
                "componentType": 5126,
                "count": all_nrm.len() / 3,
                "type": "VEC3"
            }));

            // Indices buffer view + accessor (u32)
            let idx_bv_idx = buffer_views_json.len();
            let idx_byte_offset = bin.len();
            for v in &all_idx { bin.extend_from_slice(&v.to_le_bytes()); }
            while bin.len() % 4 != 0 { bin.push(0); }
            let idx_byte_len = bin.len() - idx_byte_offset;
            buffer_views_json.push(json!({
                "buffer": 0,
                "byteOffset": idx_byte_offset,
                "byteLength": idx_byte_len,
                "target": 34963  // ELEMENT_ARRAY_BUFFER
            }));
            let idx_acc_idx = accessors_json.len();
            accessors_json.push(json!({
                "bufferView": idx_bv_idx,
                "byteOffset": 0,
                "componentType": 5125,  // UNSIGNED_INT
                "count": all_idx.len(),
                "type": "SCALAR"
            }));

            meshes_info.push(MeshInfo {
                pos_bv: pos_acc_idx,
                nrm_bv: nrm_acc_idx,
                idx_bv: idx_acc_idx,
                // Name each 3D object by its LBD resource IRI (same as the RDF
                // subject) so consumers link straight to the LBD node. Falls back
                // to the raw GUID if the element isn't in the model.
                name: object_resource_iri(model, base, flat.express_id)
                    .unwrap_or_else(|| flat.guid.clone()),
                material_idx: mat_idx,
            });
        }

        // Build glTF JSON
        let meshes_json: Vec<serde_json::Value> = meshes_info.iter().map(|m| {
            json!({
                "name": m.name,
                "primitives": [{
                    "attributes": {
                        "POSITION": m.pos_bv,
                        "NORMAL": m.nrm_bv
                    },
                    "indices": m.idx_bv,
                    "material": m.material_idx,
                    "mode": 4  // TRIANGLES
                }]
            })
        }).collect();

        let nodes_json: Vec<serde_json::Value> = (0..meshes_info.len())
            .map(|i| json!({ "mesh": i, "name": &meshes_info[i].name }))
            .collect();

        let node_indices: Vec<usize> = (0..meshes_info.len()).collect();

        let gltf_json = json!({
            "asset": { "version": "2.0", "generator": "ifc2lbd-neo" },
            "scene": 0,
            "scenes": [{ "nodes": node_indices, "name": "Scene" }],
            "nodes": nodes_json,
            "meshes": meshes_json,
            "materials": materials_json,
            "accessors": accessors_json,
            "bufferViews": buffer_views_json,
            "buffers": [{ "byteLength": bin.len() }]
        });

        let json_bytes = gltf_json.to_string().into_bytes();
        // JSON chunk must be 4-byte aligned, padded with spaces (0x20)
        let json_len = json_bytes.len();
        let json_padded_len = (json_len + 3) & !3;
        let json_padding = json_padded_len - json_len;

        // Binary chunk must be 4-byte aligned, padded with zeros
        let bin_len = bin.len();
        let bin_padded_len = (bin_len + 3) & !3;
        let bin_padding = bin_padded_len - bin_len;

        // GLB layout:
        // 12 bytes header
        // 8 bytes JSON chunk header + json_padded_len bytes JSON
        // 8 bytes BIN chunk header + bin_padded_len bytes BIN
        let total_len = 12 + 8 + json_padded_len + 8 + bin_padded_len;

        let mut out: Vec<u8> = Vec::with_capacity(total_len);
        // Header: magic 0x46546C67, version 2, total length
        out.extend_from_slice(&0x46546C67u32.to_le_bytes()); // magic "glTF"
        out.extend_from_slice(&2u32.to_le_bytes());           // version
        out.extend_from_slice(&(total_len as u32).to_le_bytes());
        // JSON chunk: length, type 0x4E4F534A ("JSON")
        out.extend_from_slice(&(json_padded_len as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F534Au32.to_le_bytes());
        out.extend_from_slice(&json_bytes);
        for _ in 0..json_padding { out.push(0x20); } // space padding
        // Binary chunk: length, type 0x004E4942 ("BIN\0")
        out.extend_from_slice(&(bin_padded_len as u32).to_le_bytes());
        out.extend_from_slice(&0x004E4942u32.to_le_bytes());
        out.extend_from_slice(&bin);
        for _ in 0..bin_padding { out.push(0); }

        Ok(out)
    }

    fn bake_geom(mesh: &ifc_geometry::Mesh, m: &[f64; 16], pos: &mut Vec<f32>, nrm: &mut Vec<f32>, idx: &mut Vec<u32>, base_v: &mut u32) {
        let n = mesh.positions.len() / 3;
        for i in 0..n {
            let (lx, ly, lz) = (mesh.positions[i*3] as f64, mesh.positions[i*3+1] as f64, mesh.positions[i*3+2] as f64);
            pos.push((m[0]*lx + m[4]*ly + m[8]*lz + m[12]) as f32);
            pos.push((m[1]*lx + m[5]*ly + m[9]*lz + m[13]) as f32);
            pos.push((m[2]*lx + m[6]*ly + m[10]*lz + m[14]) as f32);
            if mesh.normals.len() == mesh.positions.len() {
                let (nx, ny, nz) = (mesh.normals[i*3] as f64, mesh.normals[i*3+1] as f64, mesh.normals[i*3+2] as f64);
                let (wnx, wny, wnz) = (m[0]*nx + m[4]*ny + m[8]*nz, m[1]*nx + m[5]*ny + m[9]*nz, m[2]*nx + m[6]*ny + m[10]*nz);
                let len = (wnx*wnx + wny*wny + wnz*wnz).sqrt().max(1e-12);
                nrm.push((wnx/len) as f32); nrm.push((wny/len) as f32); nrm.push((wnz/len) as f32);
            } else { nrm.push(0.0); nrm.push(0.0); nrm.push(1.0); }
        }
        for i in &mesh.indices { idx.push(*i + *base_v); }
        *base_v += n as u32;
    }
}

#[cfg(feature = "fmt-parquet")]
fn serialize_parquet(
    ctx: &PipelineContext,
    tessellated: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    use ifc_model::IfcModel;
    use ifc_step::StepFile;
    let ifc_model = ctx.get::<IfcModel>();
    let step = ctx.get::<StepFile>();
    parquet_writer::write(tessellated, ifc_model.as_deref(), step.as_deref())
        .map_err(|e| ProducerError::Conversion(format!("parquet: {e}")))
}

#[cfg(not(feature = "fmt-parquet"))]
fn serialize_parquet(
    _ctx: &PipelineContext,
    _model: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    Err(ProducerError::Conversion("parquet not compiled (enable fmt-parquet feature)".into()))
}

#[cfg(feature = "fmt-parquet")]
mod parquet_writer;

// ─── Fragments builder ────────────────────────────────────────────────────────

#[cfg(feature = "fmt-fragments")]
mod fragments {
    use flate2::{write::ZlibEncoder, Compression};
    use fragments_schema::*;
    use flatbuffers::{FlatBufferBuilder, WIPOffset};
    use ifc_model::IfcModel;
    use ifc_step::StepFile;
    use std::collections::HashMap;
    use std::io::Write;
    use tessellated_model::TessellatedModel;

    pub fn build_fragments(
        tessellated: &TessellatedModel,
        model: &IfcModel,
        step: &StepFile,
        base: &str,
    ) -> Result<Vec<u8>, String> {
        let mut builder = FlatBufferBuilder::new();

        // ThatOpen resolves element identity as local_ids[meshes_items[Sample.item]], so
        // meshes_items must carry each element's index *within local_ids*. Build that lookup
        // from the exact same entity-id ordering the entity section will use for local_ids
        // (mode-dependent), so the two stay in lockstep.
        let entity_ids: Vec<u64> = match tessellated.metadata_mode {
            tessellated_model::MetadataMode::Full => {
                fragments_core::collect_serialized_entity_ids(model, step)
            }
            tessellated_model::MetadataMode::Stripped => stripped_entity_ids(model, step),
        };
        let local_id_to_index: HashMap<u64, u32> = entity_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i as u32))
            .collect();

        // Build meshes first and store as raw u32 to break the FlatBuffers
        // lifetime chain — this lets us re-borrow builder for entity section.
        let meshes_raw = build_meshes(&mut builder, tessellated, step, &local_id_to_index)?.value();

        // ── Entity/attribute/relation data — respects metadata mode ───────────
        let entity_data = build_entity_data(&mut builder, model, step, tessellated.metadata_mode, base)?;

        // ── Model FlatBuffer ──────────────────────────────────────────────────
        // Reconstruct typed WIPOffset values from raw u32 (no lifetime constraint)
        use flatbuffers::WIPOffset;
        let model_offset = Model::create(
            &mut builder,
            &ModelArgs {
                metadata:        Some(WIPOffset::new(entity_data.metadata)),
                guids:           Some(WIPOffset::new(entity_data.guids)),
                guids_items:     Some(WIPOffset::new(entity_data.guids_items)),
                max_local_id:    entity_data.max_local_id,
                local_ids:       Some(WIPOffset::new(entity_data.local_ids)),
                categories:      Some(WIPOffset::new(entity_data.categories)),
                meshes:          Some(WIPOffset::new(meshes_raw)),
                attributes:      Some(WIPOffset::new(entity_data.attributes)),
                relations:       Some(WIPOffset::new(entity_data.relations)),
                relations_items: Some(WIPOffset::new(entity_data.relations_items)),
                guid:            Some(WIPOffset::new(entity_data.root_guid)),
                spatial_structure: entity_data.spatial_structure.map(WIPOffset::new),
                unique_attributes: None,
                relation_names:    None,
                indexes:           None,
            },
        );
        finish_model_buffer(&mut builder, model_offset);

        let raw = builder.finished_data().to_vec();
        let compressed = compress(&raw).map_err(|e| e.to_string())?;
        Ok(compressed)
    }

    // Use EntitySection from fragments_core (no lifetime — stores raw u32 offsets)
    use fragments_core::EntitySection as EntityData;

    fn build_entity_data(
        builder: &mut FlatBufferBuilder,
        model: &IfcModel,
        step: &StepFile,
        metadata_mode: tessellated_model::MetadataMode,
        base: &str,
    ) -> Result<EntityData, String> {
        use fragments_core::build_entity_section;
        use tessellated_model::MetadataMode;

        match metadata_mode {
            MetadataMode::Full => {
                build_entity_section(builder, model, step, base)
                    .map_err(|e| format!("entity section: {e}"))
            }
            MetadataMode::Stripped => {
                // Stripped: only GUIDs + empty attributes/relations/categories.
                // Elements still have GUIDs for identity; no property/relation data.
                build_stripped_entity_section(builder, model, step, base)
            }
        }
    }

    /// Ordered entity-id list backing `local_ids` in stripped mode: elements
    /// (minus IFCOPENINGELEMENT) plus spatial nodes, sorted ascending. Shared with
    /// the meshes_items index map so both stay in lockstep.
    pub fn stripped_entity_ids(model: &IfcModel, step: &StepFile) -> Vec<u64> {
        let mut element_ids: Vec<u64> = model.elements.keys().copied()
            .filter(|id| !step.entities.get(id).map(|e| e.entity_name == "IFCOPENINGELEMENT").unwrap_or(false))
            .collect();
        for id in model.spatial_nodes.keys().copied() {
            element_ids.push(id);
        }
        element_ids.sort_unstable();
        element_ids
    }

    fn build_stripped_entity_section(
        builder: &mut FlatBufferBuilder,
        model: &IfcModel,
        step: &StepFile,
        base: &str,
    ) -> Result<EntityData, String> {
        // Only serialize elements + spatial nodes (not the full entity list)
        let element_ids = stripped_entity_ids(model, step);

        let local_ids_u32: Vec<u32> = element_ids.iter().map(|&id| id as u32).collect();
        let local_ids = builder.create_vector(&local_ids_u32);

        // Per-object resource IRIs (identical to the LBD RDF subjects) so the
        // viewer links a picked object straight to its LBD node.
        let mut guid_strs: Vec<WIPOffset<&str>> = Vec::new();
        let mut guid_items: Vec<u32> = Vec::new();
        for &id in &element_ids {
            if let Some(node) = model.spatial_nodes.get(&id) {
                let iri = ifc_model::iri::spatial_node_resource_iri(base, node);
                guid_strs.push(builder.create_string(&iri));
                guid_items.push(id as u32);
            } else if let Some(node) = model.elements.get(&id) {
                let iri = ifc_model::iri::element_resource_iri(base, node);
                guid_strs.push(builder.create_string(&iri));
                guid_items.push(id as u32);
            }
        }
        let guids = builder.create_vector(&guid_strs);
        let guids_items = builder.create_vector(&guid_items);

        // Empty categories (just entity type names)
        let cat_offsets: Vec<WIPOffset<&str>> = element_ids.iter()
            .map(|id| builder.create_string(step.entities.get(id).map(|e| e.entity_name.as_str()).unwrap_or("UNKNOWN")))
            .collect();
        let categories = builder.create_vector(&cat_offsets);

        // Empty attributes (one empty Attribute per entity)
        let empty_attrs: Vec<WIPOffset<Attribute>> = element_ids.iter().map(|_| {
            let empty_data = builder.create_vector::<WIPOffset<&str>>(&[]);
            Attribute::create(builder, &AttributeArgs { data: Some(empty_data) })
        }).collect();
        let attributes = builder.create_vector(&empty_attrs);

        let empty_rels = builder.create_vector::<WIPOffset<Relation>>(&[]);
        let empty_rel_items = builder.create_vector::<i32>(&[]);

        let metadata_str = format!("{{\"schema\":\"IFC4\",\"names\":[],\"descriptions\":[],\"crs\":null}}");
        let metadata = builder.create_string(&metadata_str);
        let root_guid = model.spatial_nodes.values()
            .find(|n| step.entities.get(&n.id).map(|e| e.entity_name == "IFCPROJECT").unwrap_or(false))
            .map(|n| builder.create_string(&n.guid))
            .unwrap_or_else(|| builder.create_string("ifc2lbd-neo"));

        let max_local_id = local_ids_u32.iter().copied().max().unwrap_or(0).saturating_add(1);

        Ok(EntityData {
            metadata: metadata.value(),
            root_guid: root_guid.value(),
            guids: guids.value(),
            guids_items: guids_items.value(),
            local_ids: local_ids.value(),
            categories: categories.value(),
            attributes: attributes.value(),
            relations: empty_rels.value(),
            relations_items: empty_rel_items.value(),
            spatial_structure: None,
            max_local_id,
        })
    }

    fn build_meshes<'a>(
        builder: &mut FlatBufferBuilder<'a>,
        tessellated: &TessellatedModel,
        _step: &StepFile,
        local_id_to_index: &HashMap<u64, u32>,
    ) -> Result<flatbuffers::WIPOffset<Meshes<'a>>, String> {
        // ifc-lite-native enumeration: shells, placements and per-instance transforms come
        // straight from the TessellatedModel. Every element ifc-lite tessellated is emitted
        // (no second fragments-core STEP walk gating coverage), and shell dedup keys on the
        // position-free ifc-lite mesh content hash.

        let mut shells: Vec<flatbuffers::WIPOffset<Shell<'a>>> = Vec::new();
        let mut representations: Vec<Representation> = Vec::new();
        let mut samples: Vec<Sample> = Vec::new();
        let mut global_transforms: Vec<Transform> = Vec::new();
        let mut local_transforms: Vec<Transform> = Vec::new();
        let mut materials: Vec<Material> = Vec::new();

        let mut meshes_items: Vec<u32> = Vec::new();
        let mut sample_ids: Vec<u32> = Vec::new();
        let mut representation_ids: Vec<u32> = Vec::new();
        let mut material_ids: Vec<u32> = Vec::new();
        let mut lt_ids: Vec<u32> = Vec::new();
        let mut gt_ids: Vec<u32> = Vec::new();

        // Identity local transform at index 0 (oracle: "first local transform is no-transform")
        local_transforms.push(Transform::new(
            &DoubleVector::new(0.0, 0.0, 0.0),
            &FloatVector::new(1.0, 0.0, 0.0),
            &FloatVector::new(0.0, 1.0, 0.0),
        ));
        let mut next_id: u32 = 1;
        lt_ids.push(next_id); next_id += 1;

        // Dedup: reusable geometry identity (if available) → shell index.
        // Fallback content hash → shell index.
        // Local transform key → lt index.
        let mut shell_dedup_by_key: HashMap<u64, u32> = HashMap::new();
        let mut shell_dedup_by_hash: HashMap<u64, u32> = HashMap::new();
        let mut lt_dedup: HashMap<[u32; 9], u32> = HashMap::new();
        let mut material_dedup: HashMap<[u8; 4], u32> = HashMap::new();
        let mut item_counter = 0u32;
        let mut dbg_shells_by_cat: HashMap<String, u32> = HashMap::new();
        let normalize = std::env::var_os("IFC_NO_DEDUP_NORM").is_none();

        for flat_mesh in &tessellated.meshes {
            if flat_mesh.geometries.is_empty() { continue; }
            let element_id = flat_mesh.express_id;

            // Global transform = the element's world placement. ifc-lite stores the same
            // world_transform on every geometry of an element (element_placement × firstGeom).
            global_transforms.push(colmajor_to_transform(&flat_mesh.geometries[0].world_transform));
            gt_ids.push(element_id as u32);
            // meshes_items[Sample.item] must index into Model.local_ids (ThatOpen contract);
            // emit this element's position within local_ids, not the geometry counter.
            let local_id_index = *local_id_to_index
                .get(&element_id)
                .ok_or_else(|| format!("geometry element {element_id} missing from local_ids entity list"))?;
            meshes_items.push(local_id_index);

            for geom in &flat_mesh.geometries {
                // Per-instance translation normalization: store the shell relative to its
                // own min-corner and fold the offset back into this instance's transform.
                // Identical shapes at different positions then share one shell (key dedup
                // win for tessellated assemblies — railing balusters, door panels — whose
                // placement is baked into the vertices with no IFC Position to un-bake).
                // IFC_NO_DEDUP_NORM disables translation normalization (benchmark: measures
                // shell count/size WITHOUT the positional dedup of repeated shapes).
                let offset = if normalize { super::mesh_min_corner(&geom.mesh) } else { [0.0; 3] };

                // Shell dedup: fast path by reusable geometry identity (ifc-lite cache pointer
                // when available), then by translation-invariant content hash so
                // geometrically-identical shapes collapse to one shell.
                let repr_idx = if let Some(dedup_key) = geom.dedup_key {
                    if let Some(&existing) = shell_dedup_by_key.get(&dedup_key) {
                        existing
                    } else {
                        let shell_hash = super::hash_ifc_mesh(&geom.mesh, offset);
                        if let Some(&existing) = shell_dedup_by_hash.get(&shell_hash) {
                            shell_dedup_by_key.insert(dedup_key, existing);
                            existing
                        } else {
                            *dbg_shells_by_cat.entry(flat_mesh.category.clone()).or_insert(0u32) += 1;
                            let idx = shells.len() as u32;
                            shell_dedup_by_key.insert(dedup_key, idx);
                            shell_dedup_by_hash.insert(shell_hash, idx);

                            let shell_offset = build_shell(builder, &geom.mesh, offset);
                            // bbox of the normalized shell (relative to offset → min at origin)
                            let (bmin, bmax) = mesh_bbox(&geom.mesh);
                            let (ox, oy, oz) = (offset[0] as f32, offset[1] as f32, offset[2] as f32);
                            representations.push(Representation::new(
                                idx,
                                &BoundingBox::new(
                                    &FloatVector::new(bmin[0] - ox, bmin[1] - oy, bmin[2] - oz),
                                    &FloatVector::new(bmax[0] - ox, bmax[1] - oy, bmax[2] - oz),
                                ),
                                RepresentationClass::SHELL,
                            ));
                            representation_ids.push(next_id); next_id += 1;
                            shells.push(shell_offset);
                            idx
                        }
                    }
                } else {
                    let shell_hash = super::hash_ifc_mesh(&geom.mesh, offset);
                    if let Some(&existing) = shell_dedup_by_hash.get(&shell_hash) {
                        existing
                    } else {
                        *dbg_shells_by_cat.entry(flat_mesh.category.clone()).or_insert(0u32) += 1;
                        let idx = shells.len() as u32;
                        shell_dedup_by_hash.insert(shell_hash, idx);

                        let shell_offset = build_shell(builder, &geom.mesh, offset);
                        // bbox of the normalized shell (relative to offset → min at origin)
                        let (bmin, bmax) = mesh_bbox(&geom.mesh);
                        let (ox, oy, oz) = (offset[0] as f32, offset[1] as f32, offset[2] as f32);
                        representations.push(Representation::new(
                            idx,
                            &BoundingBox::new(
                                &FloatVector::new(bmin[0] - ox, bmin[1] - oy, bmin[2] - oz),
                                &FloatVector::new(bmax[0] - ox, bmax[1] - oy, bmax[2] - oz),
                            ),
                            RepresentationClass::SHELL,
                        ));
                        representation_ids.push(next_id); next_id += 1;
                        shells.push(shell_offset);
                        idx
                    }
                };

                // Material dedup by RGBA bytes.
                let c = geom.color;
                let color_bytes = [
                    (c[0] * 255.0) as u8,
                    (c[1] * 255.0) as u8,
                    (c[2] * 255.0) as u8,
                    (c[3] * 255.0) as u8,
                ];
                let mat_idx = *material_dedup.entry(color_bytes).or_insert_with(|| {
                    let idx = materials.len() as u32;
                    materials.push(Material::new(
                        color_bytes[0], color_bytes[1], color_bytes[2], color_bytes[3],
                        RenderedFaces::ONE, Stroke::DEFAULT,
                    ));
                    material_ids.push(next_id); next_id += 1;
                    idx
                });

                // Local transform = (firstGeom^-1 × thisGeom) × translate(offset), so the
                // min-corner-normalized shell is placed back at its true position.
                // ifc-lite stores local_transform as identity for the element's first geometry.
                let local = super::mul_mat4(&geom.local_transform, &super::translate_colmajor(offset));
                let lt_id = if colmajor_is_identity(&local) {
                    0u32
                } else {
                    let key = colmajor_transform_key(&local);
                    *lt_dedup.entry(key).or_insert_with(|| {
                        let idx = local_transforms.len() as u32;
                        local_transforms.push(colmajor_to_transform(&local));
                        lt_ids.push(next_id); next_id += 1;
                        idx
                    })
                };

                samples.push(Sample::new(item_counter, mat_idx, repr_idx, lt_id));
                sample_ids.push(next_id); next_id += 1;
            }

            item_counter += 1;
        }

        if std::env::var_os("IFC_DEDUP_STATS").is_some() {
            eprintln!(
                "[dedup] elements={} samples={} unique_shells={} (reuse {:.2}x)",
                item_counter,
                samples.len(),
                shells.len(),
                samples.len() as f64 / shells.len().max(1) as f64,
            );
            let mut cats: Vec<_> = dbg_shells_by_cat.iter().collect();
            cats.sort_by(|a, b| b.1.cmp(a.1));
            for (cat, n) in cats.iter().take(12) {
                eprintln!("[dedup]   {:>5} unique shells  {}", n, cat);
            }
        }

        let shells_offset = builder.create_vector(&shells);
        let reprs_offset = builder.create_vector(&representations);
        let samples_offset = builder.create_vector(&samples);
        let mats_offset = builder.create_vector(&materials);
        let lts_offset = builder.create_vector(&local_transforms);
        let gts_offset = builder.create_vector(&global_transforms);
        let meshes_items_offset = builder.create_vector(&meshes_items);
        let sample_ids_offset = builder.create_vector(&sample_ids);
        let repr_ids_offset = builder.create_vector(&representation_ids);
        let mat_ids_offset = builder.create_vector(&material_ids);
        let lt_ids_offset = builder.create_vector(&lt_ids);
        let gt_ids_offset = builder.create_vector(&gt_ids);
        let empty_ce = builder.create_vector::<flatbuffers::WIPOffset<CircleExtrusion>>(&[]);
        let coords = Transform::new(
            &DoubleVector::new(0.0, 0.0, 0.0),
            &FloatVector::new(1.0, 0.0, 0.0),
            &FloatVector::new(0.0, 1.0, 0.0),
        );

        Ok(Meshes::create(builder, &MeshesArgs {
            coordinates: Some(&coords),
            meshes_items: Some(meshes_items_offset),
            samples: Some(samples_offset),
            representations: Some(reprs_offset),
            materials: Some(mats_offset),
            circle_extrusions: Some(empty_ce),
            shells: Some(shells_offset),
            local_transforms: Some(lts_offset),
            global_transforms: Some(gts_offset),
            material_ids: Some(mat_ids_offset),
            representation_ids: Some(repr_ids_offset),
            sample_ids: Some(sample_ids_offset),
            local_transform_ids: Some(lt_ids_offset),
            global_transform_ids: Some(gt_ids_offset),
        }))
    }

    fn build_shell<'a>(builder: &mut FlatBufferBuilder<'a>, mesh: &ifc_geometry::Mesh, offset: [f64; 3]) -> flatbuffers::WIPOffset<Shell<'a>> {
        use fragments_core::get_raw_shell_data;

        // Store vertices relative to `offset` (the mesh min-corner) so identical shapes at
        // different positions share one shell; the offset is re-applied per instance via the
        // sample's local transform.
        let (ox, oy, oz) = (offset[0] as f32, offset[1] as f32, offset[2] as f32);
        let positions: Vec<[f32; 3]> = mesh.positions.chunks_exact(3)
            .map(|c| [c[0] - ox, c[1] - oy, c[2] - oz])
            .collect();
        let triangles: Vec<[u32; 3]> = mesh.indices.chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();

        // Raw per-triangle shells: faithful to ifc-lite's triangulation (same geometry the
        // validated glTF path emits). The coplanar `get_shell_data` boundary tracer cannot
        // robustly reconstruct face loops from ifc-lite's arbitrary triangulations and
        // produced spanning-polygon garbage; raw mode is correct by construction.
        let shell_data = get_raw_shell_data(&positions, &triangles);
        let is_big = shell_data.points.len() > 65000;

        let point_vec: Vec<FloatVector> = shell_data.points.iter()
            .map(|p| FloatVector::new(p[0], p[1], p[2]))
            .collect();
        let points_offset = builder.create_vector(&point_vec);
        let face_ids_offset = builder.create_vector(&shell_data.profiles_face_ids);

        let mut sorted_keys: Vec<usize> = shell_data.profiles.keys().copied().collect();
        sorted_keys.sort_unstable();

        if is_big {
            let mut big_profiles: Vec<flatbuffers::WIPOffset<BigShellProfile>> = Vec::new();
            for key in &sorted_keys {
                let p = &shell_data.profiles[key];
                let idx = builder.create_vector(p.as_slice());
                big_profiles.push(BigShellProfile::create(builder, &BigShellProfileArgs { indices: Some(idx) }));
            }
            let mut big_holes: Vec<flatbuffers::WIPOffset<BigShellHole>> = Vec::new();
            let mut hole_keys: Vec<usize> = shell_data.holes.keys().copied().collect();
            hole_keys.sort_unstable();
            for hk in hole_keys {
                for hole in &shell_data.holes[&hk] {
                    let idx = builder.create_vector(hole.as_slice());
                    big_holes.push(BigShellHole::create(builder, &BigShellHoleArgs { indices: Some(idx), profile_id: hk as u16 }));
                }
            }
            let empty_profiles = builder.create_vector::<flatbuffers::WIPOffset<ShellProfile>>(&[]);
            let empty_holes = builder.create_vector::<flatbuffers::WIPOffset<ShellHole>>(&[]);
            let big_profiles_v = builder.create_vector(&big_profiles);
            let big_holes_v = builder.create_vector(&big_holes);
            Shell::create(builder, &ShellArgs {
                profiles: Some(empty_profiles),
                holes: Some(empty_holes),
                points: Some(points_offset),
                big_profiles: Some(big_profiles_v),
                big_holes: Some(big_holes_v),
                type_: ShellType::BIG,
                profiles_face_ids: Some(face_ids_offset),
            })
        } else {
            let mut profiles: Vec<flatbuffers::WIPOffset<ShellProfile>> = Vec::new();
            for key in &sorted_keys {
                let p = &shell_data.profiles[key];
                let indices: Vec<u16> = p.iter().filter_map(|&v| u16::try_from(v).ok()).collect();
                let idx = builder.create_vector(&indices);
                profiles.push(ShellProfile::create(builder, &ShellProfileArgs { indices: Some(idx) }));
            }
            let mut holes: Vec<flatbuffers::WIPOffset<ShellHole>> = Vec::new();
            let mut hole_keys: Vec<usize> = shell_data.holes.keys().copied().collect();
            hole_keys.sort_unstable();
            for hk in hole_keys {
                for hole in &shell_data.holes[&hk] {
                    let indices: Vec<u16> = hole.iter().filter_map(|&v| u16::try_from(v).ok()).collect();
                    let idx = builder.create_vector(&indices);
                    holes.push(ShellHole::create(builder, &ShellHoleArgs { indices: Some(idx), profile_id: hk as u16 }));
                }
            }
            let profiles_v = builder.create_vector(&profiles);
            let holes_v = builder.create_vector(&holes);
            let empty_big_p = builder.create_vector::<flatbuffers::WIPOffset<BigShellProfile>>(&[]);
            let empty_big_h = builder.create_vector::<flatbuffers::WIPOffset<BigShellHole>>(&[]);
            Shell::create(builder, &ShellArgs {
                profiles: Some(profiles_v),
                holes: Some(holes_v),
                points: Some(points_offset),
                big_profiles: Some(empty_big_p),
                big_holes: Some(empty_big_h),
                type_: ShellType::NONE,
                profiles_face_ids: Some(face_ids_offset),
            })
        }
    }

    /// Oracle-style content hash from ifc-file-reader.ts.
    ///
    fn mesh_bbox(mesh: &ifc_geometry::Mesh) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for chunk in mesh.positions.chunks_exact(3) {
            for i in 0..3 {
                min[i] = min[i].min(chunk[i]);
                max[i] = max[i].max(chunk[i]);
            }
        }
        (min, max)
    }

    /// Build a fragments `Transform` (translation + X/Y basis axes; Z derived by the
    /// viewer) from a column-major 4×4 matrix. Translation kept in f64, axes in f32.
    fn colmajor_to_transform(m: &[f64; 16]) -> Transform {
        Transform::new(
            &DoubleVector::new(m[12], m[13], m[14]),
            &FloatVector::new(m[0] as f32, m[1] as f32, m[2] as f32),
            &FloatVector::new(m[4] as f32, m[5] as f32, m[6] as f32),
        )
    }

    /// True when a column-major 4×4 is the identity (within fp tolerance).
    fn colmajor_is_identity(m: &[f64; 16]) -> bool {
        const ID: [f64; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        m.iter().zip(ID.iter()).all(|(a, b)| (a - b).abs() < 1e-9)
    }

    /// 9-component dedup key (translation + X/Y axes as f32 bits) for a column-major matrix.
    fn colmajor_transform_key(m: &[f64; 16]) -> [u32; 9] {
        [
            (m[12] as f32).to_bits(), (m[13] as f32).to_bits(), (m[14] as f32).to_bits(),
            (m[0] as f32).to_bits(), (m[1] as f32).to_bits(), (m[2] as f32).to_bits(),
            (m[4] as f32).to_bits(), (m[5] as f32).to_bits(), (m[6] as f32).to_bits(),
        ]
    }

    fn compress(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(bytes)?;
        enc.finish()
    }
}
