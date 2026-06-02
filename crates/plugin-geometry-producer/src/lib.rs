//! Geometry producer plugin.
//!
//! Reads TessellatedModel from context, serializes to chosen format, emits
//! via sidecar_tx. Supported formats: fragments (default), gltf, parquet, ifc5.

use std::sync::Arc;

use crossbeam::channel::Sender;
use lbd_pipeline::{
    DerivedFile, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};
use tessellated_model::TessellatedModel;

pub const GEOMETRY_PRODUCER_ID: &str = "neo-geometry-producer";

/// Runtime config stored in PipelineContext by the CLI/WASM runner.
#[derive(Clone, Copy)]
pub struct GeometryProducerConfig {
    pub format: GeometryFormat,
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

    fragments::build_fragments(model, &ifc_model, &step)
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
    _ctx: &PipelineContext,
    tessellated: &TessellatedModel,
) -> Result<Vec<u8>, ProducerError> {
    gltf_writer::write(tessellated)
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
    use std::io::{Cursor, Write as IoWrite};
    use serde_json::json;

    pub fn write(tessellated: &TessellatedModel) -> Result<Vec<u8>, String> {
        let mut bin: Vec<u8> = Vec::new();

        // Each accessor (one buffer view each): positions, normals, indices
        struct MeshInfo {
            pos_bv: usize,   // buffer view index for positions
            nrm_bv: usize,   // buffer view index for normals
            idx_bv: usize,   // buffer view index for indices
            idx_count: usize,
            pos_min: [f32; 3],
            pos_max: [f32; 3],
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

            // Merge all sub-meshes for this element
            let mut all_pos: Vec<f32> = Vec::new();
            let mut all_nrm: Vec<f32> = Vec::new();
            let mut all_idx: Vec<u32> = Vec::new();
            let mut base_v: u32 = 0;
            let first_color = flat.geometries[0].color;

            for geom in &flat.geometries {
                let pos = &geom.mesh.positions;
                let nrm = &geom.mesh.normals;
                let n = pos.len() / 3;
                let m = &geom.world_transform;

                for i in 0..n {
                    let (lx, ly, lz) = (pos[i*3] as f64, pos[i*3+1] as f64, pos[i*3+2] as f64);
                    all_pos.push((m[0]*lx + m[4]*ly + m[8]*lz + m[12]) as f32);
                    all_pos.push((m[1]*lx + m[5]*ly + m[9]*lz + m[13]) as f32);
                    all_pos.push((m[2]*lx + m[6]*ly + m[10]*lz + m[14]) as f32);
                    if nrm.len() == pos.len() {
                        let (nx, ny, nz) = (nrm[i*3] as f64, nrm[i*3+1] as f64, nrm[i*3+2] as f64);
                        let wnx = m[0]*nx + m[4]*ny + m[8]*nz;
                        let wny = m[1]*nx + m[5]*ny + m[9]*nz;
                        let wnz = m[2]*nx + m[6]*ny + m[10]*nz;
                        let len = (wnx*wnx + wny*wny + wnz*wnz).sqrt().max(1e-12);
                        all_nrm.push((wnx/len) as f32);
                        all_nrm.push((wny/len) as f32);
                        all_nrm.push((wnz/len) as f32);
                    } else {
                        all_nrm.push(0.0); all_nrm.push(0.0); all_nrm.push(1.0);
                    }
                }
                for idx in &geom.mesh.indices { all_idx.push(*idx + base_v); }
                base_v += (pos.len() / 3) as u32;
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
                idx_count: all_idx.len(),
                pos_min: min,
                pos_max: max,
                name: flat.guid.clone(),
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
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use fragments_schema::*;
    use flatbuffers::{FlatBufferBuilder, WIPOffset};
    use ifc_model::IfcModel;
    use ifc_step::{StepFile, StepValue};
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    use std::io::Write;
    use tessellated_model::TessellatedModel;

    pub fn build_fragments(
        tessellated: &TessellatedModel,
        model: &IfcModel,
        step: &StepFile,
    ) -> Result<Vec<u8>, String> {
        let mut builder = FlatBufferBuilder::new();

        // Build meshes first and store as raw u32 to break the FlatBuffers
        // lifetime chain — this lets us re-borrow builder for entity section.
        let meshes_raw = build_meshes(&mut builder, tessellated)?.value();

        // ── Entity/attribute/relation data — respects metadata mode ───────────
        let entity_data = build_entity_data(&mut builder, model, step, tessellated.metadata_mode)?;

        // Reconstruct meshes offset (WIPOffset is just a u32 wrapper)
        let meshes: flatbuffers::WIPOffset<Meshes> = flatbuffers::WIPOffset::new(meshes_raw);

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
    ) -> Result<EntityData, String> {
        use fragments_core::build_entity_section;
        use tessellated_model::MetadataMode;

        match metadata_mode {
            MetadataMode::Full => {
                build_entity_section(builder, model, step)
                    .map_err(|e| format!("entity section: {e}"))
            }
            MetadataMode::Stripped => {
                // Stripped: only GUIDs + empty attributes/relations/categories.
                // Elements still have GUIDs for identity; no property/relation data.
                build_stripped_entity_section(builder, model, step)
            }
        }
    }

    fn build_stripped_entity_section(
        builder: &mut FlatBufferBuilder,
        model: &IfcModel,
        step: &StepFile,
    ) -> Result<EntityData, String> {
        // Only serialize elements + spatial nodes (not the full entity list)
        let mut element_ids: Vec<u64> = model.elements.keys().copied()
            .filter(|id| !step.entities.get(id).map(|e| e.entity_name == "IFCOPENINGELEMENT").unwrap_or(false))
            .collect();
        for id in model.spatial_nodes.keys().copied() {
            element_ids.push(id);
        }
        element_ids.sort_unstable();

        let local_ids_u32: Vec<u32> = element_ids.iter().map(|&id| id as u32).collect();
        let local_ids = builder.create_vector(&local_ids_u32);

        // GUIDs
        let mut guid_strs: Vec<WIPOffset<&str>> = Vec::new();
        let mut guid_items: Vec<u32> = Vec::new();
        for &id in &element_ids {
            if let Some(node) = model.spatial_nodes.get(&id) {
                guid_strs.push(builder.create_string(&node.guid));
                guid_items.push(id as u32);
            } else if let Some(node) = model.elements.get(&id) {
                guid_strs.push(builder.create_string(&node.guid));
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
    ) -> Result<flatbuffers::WIPOffset<Meshes<'a>>, String> {
        use ifc_geometry::Mesh as IFCMesh;

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

        // Dual dedup matching oracle's approach:
        // 1. By geometry entity ID (fast, covers IFCMAPPEDITEM shared geometry)
        // 2. By content hash (covers different STEP entities with identical geometry)
        let mut shell_dedup_by_id: HashMap<u64, u32> = HashMap::new();
        let mut shell_dedup_by_hash: HashMap<u64, u32> = HashMap::new();
        let mut material_dedup: HashMap<[u8; 4], u32> = HashMap::new();
        let mut item_counter = 0u32;

        for flat_mesh in &tessellated.meshes {
            if flat_mesh.geometries.is_empty() { continue; }

            let first_geom = &flat_mesh.geometries[0];

            // Global transform from first geometry world matrix (column-major)
            let m = &first_geom.world_transform;
            global_transforms.push(Transform::new(
                &DoubleVector::new(m[12], m[13], m[14]),
                &FloatVector::new(m[0] as f32, m[1] as f32, m[2] as f32),
                &FloatVector::new(m[4] as f32, m[5] as f32, m[6] as f32),
            ));
            gt_ids.push(flat_mesh.express_id as u32);
            meshes_items.push(item_counter);

            for (geom_idx, geom) in flat_mesh.geometries.iter().enumerate() {
                // Dedup: entity ID first, then content hash (oracle: _previousGeometriesIDs + _previousGeometries)
                let repr_idx = if let Some(&existing) = shell_dedup_by_id.get(&geom.geometry_id) {
                    existing
                } else {
                    let mesh_hash = hash_ifc_mesh(&geom.mesh);
                    if let Some(&existing) = shell_dedup_by_hash.get(&mesh_hash) {
                        shell_dedup_by_id.insert(geom.geometry_id, existing);
                        existing
                    } else {
                    let idx = shells.len() as u32;
                    shell_dedup_by_id.insert(geom.geometry_id, idx);
                    shell_dedup_by_hash.insert(mesh_hash, idx);

                    // Build Shell FlatBuffer from ifc-lite Mesh
                    let shell_offset = build_shell(builder, &geom.mesh);
                    let (bbox_min, bbox_max) = mesh_bbox(&geom.mesh);
                    representations.push(Representation::new(
                        idx,
                        &BoundingBox::new(
                            &FloatVector::new(bbox_min[0], bbox_min[1], bbox_min[2]),
                            &FloatVector::new(bbox_max[0], bbox_max[1], bbox_max[2]),
                        ),
                        RepresentationClass::SHELL,
                    ));
                    representation_ids.push(next_id); next_id += 1;
                    shells.push(shell_offset);
                    idx
                    } // end else (content hash miss)
                }; // end if let (entity ID dedup)

                // Material dedup by RGBA
                let color_bytes = [
                    (geom.color[0] * 255.0) as u8,
                    (geom.color[1] * 255.0) as u8,
                    (geom.color[2] * 255.0) as u8,
                    (geom.color[3] * 255.0) as u8,
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

                // Local transform (0 = identity for first geometry)
                let lt_id = if geom_idx == 0 { 0u32 } else {
                    // TODO: compute relative local transform when ifc-lite exposes per-instance transforms
                    0u32
                };

                samples.push(Sample::new(item_counter, mat_idx, repr_idx, lt_id));
                sample_ids.push(next_id); next_id += 1;
            }

            item_counter += 1;
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

    fn build_shell<'a>(builder: &mut FlatBufferBuilder<'a>, mesh: &ifc_geometry::Mesh) -> flatbuffers::WIPOffset<Shell<'a>> {
        use fragments_core::get_shell_data;

        // Convert flat ifc-lite buffers to slice arrays for get_shell_data
        let positions: Vec<[f32; 3]> = mesh.positions.chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        let normals: Vec<[f32; 3]> = if mesh.normals.len() == mesh.positions.len() {
            mesh.normals.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
        } else {
            // Compute normals from triangles if not provided
            let mut norms = vec![[0.0f32; 3]; positions.len()];
            for tri in mesh.indices.chunks_exact(3) {
                let (i1, i2, i3) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                if i1 >= positions.len() || i2 >= positions.len() || i3 >= positions.len() { continue; }
                let (p1, p2, p3) = (positions[i1], positions[i2], positions[i3]);
                let ab = [p2[0]-p1[0], p2[1]-p1[1], p2[2]-p1[2]];
                let ac = [p3[0]-p1[0], p3[1]-p1[1], p3[2]-p1[2]];
                let n = [ab[1]*ac[2]-ab[2]*ac[1], ab[2]*ac[0]-ab[0]*ac[2], ab[0]*ac[1]-ab[1]*ac[0]];
                for &vi in &[i1, i2, i3] {
                    norms[vi][0] += n[0]; norms[vi][1] += n[1]; norms[vi][2] += n[2];
                }
            }
            for n in &mut norms {
                let len = (n[0]*n[0]+n[1]*n[1]+n[2]*n[2]).sqrt();
                if len > 1e-12 { n[0] /= len; n[1] /= len; n[2] /= len; }
            }
            norms
        };
        let triangles: Vec<[u32; 3]> = mesh.indices.chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();

        // Run oracle's getShellData for coplanar-face grouping
        let shell_data = get_shell_data(&positions, &normals, &triangles);
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

    /// Port of oracle's dedup hash from ifc-file-reader.ts.
    ///
    /// Hash string (oracle): `${vertexCount}-${triangleCount}-${hashAreaSum}-${hashBigArea}-${hashVolume}-${cx}-${cy}-${cz}-${x1}-${y1}-${z1}`
    /// All floats rounded to precision p = 10000.
    /// centroid divided by index.length (= triangle_count * 3).
    /// volume = abs(sum of signed tetrahedron volumes per triangle).
    fn hash_ifc_mesh(mesh: &ifc_geometry::Mesh) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let p = 10000.0f32;
        let round = |v: f32| -> i32 { (v * p).round() as i32 };

        let vertex_count = mesh.positions.len() / 3;
        let triangle_count = mesh.indices.len() / 3;
        let pos = &mesh.positions;
        let idx = &mesh.indices;

        let mut area_sum = 0.0f32;
        let mut biggest_area = 0.0f32;
        let mut volume = 0.0f32;
        let mut cx = 0.0f32;
        let mut cy = 0.0f32;
        let mut cz = 0.0f32;

        for tri in idx.chunks_exact(3) {
            let (i1, i2, i3) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if i1*3+2 >= pos.len() || i2*3+2 >= pos.len() || i3*3+2 >= pos.len() { continue; }
            let (ax, ay, az) = (pos[i1*3], pos[i1*3+1], pos[i1*3+2]);
            let (bx, by, bz) = (pos[i2*3], pos[i2*3+1], pos[i2*3+2]);
            let (px, py, pz) = (pos[i3*3], pos[i3*3+1], pos[i3*3+2]);

            // Area via cross product (oracle: triangle.getArea())
            let abx = bx-ax; let aby = by-ay; let abz = bz-az;
            let acx = px-ax; let acy = py-ay; let acz = pz-az;
            let cross_len = ((aby*acz - abz*acy).powi(2) + (abz*acx - abx*acz).powi(2) + (abx*acy - aby*acx).powi(2)).sqrt();
            let area = cross_len * 0.5;
            area_sum += area;
            if area > biggest_area { biggest_area = area; }

            // Centroid (oracle: centroid.add(v1, v2, v3); centroid.divideScalar(index.length))
            cx += ax + bx + px;
            cy += ay + by + py;
            cz += az + bz + pz;

            // Signed volume of tetrahedron (oracle: getSignedVolumeOfTriangle)
            // v321, v231, v312, v132, v213, v123
            let v321 = px*by*az;
            let v231 = bx*py*az;
            let v312 = px*ay*bz;
            let v132 = ax*py*bz;
            let v213 = bx*ay*pz;
            let v123 = ax*by*pz;
            volume += (1.0/6.0) * (-v321 + v231 + v312 - v132 - v213 + v123);
        }

        let n = (triangle_count * 3) as f32;
        if n > 0.0 { cx /= n; cy /= n; cz /= n; }
        volume = volume.abs();
        let (x1, y1, z1) = if vertex_count > 0 { (pos[0], pos[1], pos[2]) } else { (0.0, 0.0, 0.0) };

        let mut s = DefaultHasher::new();
        vertex_count.hash(&mut s);
        triangle_count.hash(&mut s);
        round(area_sum).hash(&mut s);
        round(biggest_area).hash(&mut s);
        round(volume).hash(&mut s);
        round(cx).hash(&mut s);
        round(cy).hash(&mut s);
        round(cz).hash(&mut s);
        round(x1).hash(&mut s);
        round(y1).hash(&mut s);
        round(z1).hash(&mut s);
        s.finish()
    }

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

    fn compress(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(bytes)?;
        enc.finish()
    }
}

