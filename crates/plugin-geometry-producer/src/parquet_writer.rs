//! Port of ifc-lite's parquet-exporter.ts — exact schema match.
//!
//! Output: ZIP archive (.bos) with multiple Parquet files.
//! Compression: Snappy (matching ifc-lite's parquet-wasm default).
//!
//! Full mode ZIP:
//!   Entities.parquet      — ExpressId, GlobalId, Name, Description, Type, HasGeometry
//!   Properties.parquet    — EntityId, PsetName, PsetGlobalId, PropName, PropType, ValueString, ValueReal, ValueInt
//!   Quantities.parquet    — EntityId, QsetName, QuantityName, QuantityType, Value
//!   Relationships.parquet — SourceId, TargetId, RelType, RelId
//!   Strings.parquet       — Index, Value (string dictionary)
//!   VertexBuffer.parquet  — X, Y, Z, NormalX, NormalY, NormalZ (per-vertex world-space)
//!   IndexBuffer.parquet   — Index0, Index1, Index2 (per-triangle, global indices)
//!   Meshes.parquet        — ExpressId, VertexStart, VertexCount, IndexStart, IndexCount
//!   Metadata.json         — version, schema, counts
//!
//! Stripped mode ZIP: VertexBuffer + IndexBuffer + Meshes + Entities (ExpressId+GlobalId only)

use arrow_array::{
    BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array, RecordBatch,
    StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::io::{Cursor, Write as IoWrite};
use std::sync::Arc;
use tessellated_model::{MetadataMode, TessellatedModel};
use zip::write::{FileOptions, ZipWriter};
use zip::CompressionMethod;

pub fn write(
    tessellated: &TessellatedModel,
    ifc_model: Option<&IfcModel>,
    step: Option<&StepFile>,
) -> Result<Vec<u8>, String> {
    let include_metadata = matches!(tessellated.metadata_mode, MetadataMode::Full);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build(),
    );

    // ── Geometry data ──────────────────────────────────────────────────────────
    let mut all_x: Vec<f32> = Vec::new();
    let mut all_y: Vec<f32> = Vec::new();
    let mut all_z: Vec<f32> = Vec::new();
    let mut all_nx: Vec<f32> = Vec::new();
    let mut all_ny: Vec<f32> = Vec::new();
    let mut all_nz: Vec<f32> = Vec::new();
    let mut all_i0: Vec<u32> = Vec::new();
    let mut all_i1: Vec<u32> = Vec::new();
    let mut all_i2: Vec<u32> = Vec::new();

    let mut mesh_entries: Vec<MeshEntry> = Vec::new();
    // Global vertex base — indices reference into the global VertexBuffer (matching ifc-lite schema)
    let mut base_v: u32 = 0;

    for flat in &tessellated.meshes {
        if flat.geometries.is_empty() {
            continue;
        }
        let vertex_start = all_x.len() as i64;
        let index_start = all_i0.len() as i64;

        for geom in &flat.geometries {
            let pos = &geom.mesh.positions;
            let nrm = &geom.mesh.normals;
            let n = pos.len() / 3;
            // Combine world_transform * local_transform so sub-meshes beyond the first
            // (e.g. IFCMAPPEDITEM instances) land at their correct world-space positions.
            let combined = crate::mul_mat4(&geom.world_transform, &geom.local_transform);
            let m = &combined;

            for i in 0..n {
                let (lx, ly, lz) = (pos[i * 3] as f64, pos[i * 3 + 1] as f64, pos[i * 3 + 2] as f64);
                all_x.push((m[0] * lx + m[4] * ly + m[8] * lz + m[12]) as f32);
                all_y.push((m[1] * lx + m[5] * ly + m[9] * lz + m[13]) as f32);
                all_z.push((m[2] * lx + m[6] * ly + m[10] * lz + m[14]) as f32);

                if nrm.len() == pos.len() {
                    let (nx, ny, nz) = (nrm[i * 3] as f64, nrm[i * 3 + 1] as f64, nrm[i * 3 + 2] as f64);
                    let wnx = m[0] * nx + m[4] * ny + m[8] * nz;
                    let wny = m[1] * nx + m[5] * ny + m[9] * nz;
                    let wnz = m[2] * nx + m[6] * ny + m[10] * nz;
                    let len = (wnx * wnx + wny * wny + wnz * wnz).sqrt().max(1e-12);
                    all_nx.push((wnx / len) as f32);
                    all_ny.push((wny / len) as f32);
                    all_nz.push((wnz / len) as f32);
                } else {
                    all_nx.push(0.0);
                    all_ny.push(0.0);
                    all_nz.push(1.0);
                }
            }

            for tri in geom.mesh.indices.chunks_exact(3) {
                all_i0.push(tri[0] + base_v);
                all_i1.push(tri[1] + base_v);
                all_i2.push(tri[2] + base_v);
            }
            base_v += n as u32;
        }

        mesh_entries.push(MeshEntry {
            express_id: flat.express_id as i64,
            guid: flat.guid.clone(),
            category: flat.category.clone(),
            vertex_start,
            vertex_count: all_x.len() as i64 - vertex_start,
            index_start,
            index_count: all_i0.len() as i64 - index_start,
        });
    }

    // ── Write Parquet files ────────────────────────────────────────────────────
    let vertex_pq = write_vertex_buffer(&all_x, &all_y, &all_z, &all_nx, &all_ny, &all_nz, &props)?;
    let index_pq = write_index_buffer(&all_i0, &all_i1, &all_i2, &props)?;
    let meshes_pq = write_meshes_table(&mesh_entries, &props)?;
    let entities_pq = write_entities_table(&mesh_entries, include_metadata, ifc_model, step, &props)?;

    // ── ZIP archive ────────────────────────────────────────────────────────────
    let mut zip_buf: Vec<u8> = Vec::new();
    {
        let cursor = Cursor::new(&mut zip_buf);
        let mut zip = ZipWriter::new(cursor);
        let opts: FileOptions<()> = FileOptions::default().compression_method(CompressionMethod::Deflated);

        // Geometry tables (always present)
        zip.start_file("VertexBuffer.parquet", opts).map_err(|e| e.to_string())?;
        zip.write_all(&vertex_pq).map_err(|e| e.to_string())?;
        zip.start_file("IndexBuffer.parquet", opts).map_err(|e| e.to_string())?;
        zip.write_all(&index_pq).map_err(|e| e.to_string())?;
        zip.start_file("Meshes.parquet", opts).map_err(|e| e.to_string())?;
        zip.write_all(&meshes_pq).map_err(|e| e.to_string())?;

        // Entity identity (always present, depth varies by mode)
        zip.start_file("Entities.parquet", opts).map_err(|e| e.to_string())?;
        zip.write_all(&entities_pq).map_err(|e| e.to_string())?;

        // Full metadata tables (only in Full mode)
        if include_metadata {
            if let (Some(model), Some(s)) = (ifc_model, step) {
                let props_pq = write_properties_table(model, s, &props)?;
                zip.start_file("Properties.parquet", opts).map_err(|e| e.to_string())?;
                zip.write_all(&props_pq).map_err(|e| e.to_string())?;

                let quant_pq = write_quantities_table(model, s, &props)?;
                zip.start_file("Quantities.parquet", opts).map_err(|e| e.to_string())?;
                zip.write_all(&quant_pq).map_err(|e| e.to_string())?;

                let rel_pq = write_relationships_table(model, &props)?;
                zip.start_file("Relationships.parquet", opts).map_err(|e| e.to_string())?;
                zip.write_all(&rel_pq).map_err(|e| e.to_string())?;
            }

            // Metadata.json
            let meta = serde_json::json!({
                "version": "2.0.0",
                "generator": "ifc2lbd-neo",
                "entityCount": mesh_entries.len(),
            });
            zip.start_file("Metadata.json", opts).map_err(|e| e.to_string())?;
            zip.write_all(meta.to_string().as_bytes()).map_err(|e| e.to_string())?;
        }

        zip.finish().map_err(|e| e.to_string())?;
    }
    Ok(zip_buf)
}

// ─── Parquet helpers ──────────────────────────────────────────────────────────

fn write_parquet<F>(schema: Arc<Schema>, props: &WriterProperties, fill: F) -> Result<Vec<u8>, String>
where
    F: FnOnce(&mut ArrowWriter<Cursor<Vec<u8>>>) -> Result<(), String>,
{
    let buf = Vec::new();
    let mut w = ArrowWriter::try_new(Cursor::new(buf), schema, Some(props.clone()))
        .map_err(|e| e.to_string())?;
    fill(&mut w)?;
    Ok(w.into_inner().map_err(|e| e.to_string())?.into_inner())
}

fn write_vertex_buffer(x: &[f32], y: &[f32], z: &[f32], nx: &[f32], ny: &[f32], nz: &[f32], props: &WriterProperties) -> Result<Vec<u8>, String> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("X", DataType::Float32, false),
        Field::new("Y", DataType::Float32, false),
        Field::new("Z", DataType::Float32, false),
        Field::new("NormalX", DataType::Float32, false),
        Field::new("NormalY", DataType::Float32, false),
        Field::new("NormalZ", DataType::Float32, false),
    ]));
    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(Float32Array::from(x.to_vec())),
            Arc::new(Float32Array::from(y.to_vec())),
            Arc::new(Float32Array::from(z.to_vec())),
            Arc::new(Float32Array::from(nx.to_vec())),
            Arc::new(Float32Array::from(ny.to_vec())),
            Arc::new(Float32Array::from(nz.to_vec())),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

fn write_index_buffer(i0: &[u32], i1: &[u32], i2: &[u32], props: &WriterProperties) -> Result<Vec<u8>, String> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("Index0", DataType::UInt32, false),
        Field::new("Index1", DataType::UInt32, false),
        Field::new("Index2", DataType::UInt32, false),
    ]));
    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(UInt32Array::from(i0.to_vec())),
            Arc::new(UInt32Array::from(i1.to_vec())),
            Arc::new(UInt32Array::from(i2.to_vec())),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

struct MeshEntry {
    express_id: i64,
    guid: String,
    category: String,
    vertex_start: i64,
    vertex_count: i64,
    index_start: i64,
    index_count: i64,
}

fn write_meshes_table(entries: &[MeshEntry], props: &WriterProperties) -> Result<Vec<u8>, String> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("ExpressId",   DataType::Int64, false),
        Field::new("VertexStart", DataType::Int64, false),
        Field::new("VertexCount", DataType::Int64, false),
        Field::new("IndexStart",  DataType::Int64, false),
        Field::new("IndexCount",  DataType::Int64, false),
    ]));
    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(Int64Array::from(entries.iter().map(|e| e.express_id).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(entries.iter().map(|e| e.vertex_start).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(entries.iter().map(|e| e.vertex_count).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(entries.iter().map(|e| e.index_start).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(entries.iter().map(|e| e.index_count).collect::<Vec<_>>())),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

fn write_entities_table(
    entries: &[MeshEntry],
    include_metadata: bool,
    _ifc_model: Option<&IfcModel>,
    _step: Option<&StepFile>,
    props: &WriterProperties,
) -> Result<Vec<u8>, String> {
    if include_metadata {
        // Full mode: ExpressId, GlobalId, Type, HasGeometry
        let schema = Arc::new(Schema::new(vec![
            Field::new("ExpressId",   DataType::Int64, false),
            Field::new("GlobalId",    DataType::Utf8,  false),
            Field::new("Type",        DataType::Utf8,  false),
            Field::new("HasGeometry", DataType::Boolean, false),
        ]));
        write_parquet(schema.clone(), props, |w| {
            let b = RecordBatch::try_new(schema, vec![
                Arc::new(Int64Array::from(entries.iter().map(|e| e.express_id).collect::<Vec<_>>())),
                Arc::new(StringArray::from(entries.iter().map(|e| e.guid.as_str()).collect::<Vec<_>>())),
                Arc::new(StringArray::from(entries.iter().map(|e| e.category.as_str()).collect::<Vec<_>>())),
                Arc::new(BooleanArray::from(vec![true; entries.len()])),
            ]).map_err(|e| e.to_string())?;
            w.write(&b).map_err(|e| e.to_string())
        })
    } else {
        // Stripped mode: ExpressId + GlobalId only (no Type, no metadata)
        let schema = Arc::new(Schema::new(vec![
            Field::new("ExpressId", DataType::Int64, false),
            Field::new("GlobalId",  DataType::Utf8,  false),
        ]));
        write_parquet(schema.clone(), props, |w| {
            let b = RecordBatch::try_new(schema, vec![
                Arc::new(Int64Array::from(entries.iter().map(|e| e.express_id).collect::<Vec<_>>())),
                Arc::new(StringArray::from(entries.iter().map(|e| e.guid.as_str()).collect::<Vec<_>>())),
            ]).map_err(|e| e.to_string())?;
            w.write(&b).map_err(|e| e.to_string())
        })
    }
}

fn write_properties_table(model: &IfcModel, _step: &StepFile, props: &WriterProperties) -> Result<Vec<u8>, String> {
    use ifc_step::StepValue;

    let mut entity_ids: Vec<i64> = Vec::new();
    let mut pset_names: Vec<Option<String>> = Vec::new();
    let mut pset_guids: Vec<Option<String>> = Vec::new();
    let mut prop_names: Vec<Option<String>> = Vec::new();
    let mut prop_types: Vec<String> = Vec::new();
    let mut value_strings: Vec<Option<String>> = Vec::new();
    let mut value_reals: Vec<f64> = Vec::new();
    let mut value_ints: Vec<i32> = Vec::new();

    for (pset_id, pset) in &model.property_sets {
        let linked_elements: Vec<u64> = model.rel_defines_by_properties
            .iter()
            .filter(|r| r.relating_property_definition == *pset_id)
            .flat_map(|r| &r.related_objects)
            .copied()
            .collect();

        for element_id in linked_elements {
            for prop_id in &pset.properties {
                if let Some(prop) = model.property_single_values.get(prop_id) {
                    entity_ids.push(element_id as i64);
                    pset_names.push(pset.name.as_ref().map(|s| s.to_string()));
                    pset_guids.push(Some(pset.guid.to_string()));
                    prop_names.push(Some(prop.name.to_string()));

                    let (ptype, vstr, vreal, vint) = match &prop.nominal_value {
                        Some(StepValue::String(s)) => ("string".into(), Some(s.to_string()), 0.0f64, 0i32),
                        Some(StepValue::Real(r)) => ("real".into(), None, *r, 0),
                        Some(StepValue::Int(i)) => ("integer".into(), None, *i as f64, *i as i32),
                        Some(StepValue::Bool(b)) => ("boolean".into(), Some(b.to_string()), if *b {1.0} else {0.0}, if *b {1} else {0}),
                        Some(StepValue::Enum(e)) => ("enum".into(), Some(e.to_string()), 0.0, 0),
                        Some(StepValue::Typed { value, .. }) => {
                            match value.as_ref() {
                                StepValue::Real(r) => ("measure".into(), None, *r, 0),
                                StepValue::Int(i) => ("measure".into(), None, *i as f64, *i as i32),
                                StepValue::String(s) => ("string".into(), Some(s.to_string()), 0.0, 0),
                                StepValue::Bool(b) => ("boolean".into(), Some(b.to_string()), if *b {1.0} else {0.0}, if *b {1} else {0}),
                                _ => ("unknown".into(), None, 0.0, 0),
                            }
                        }
                        _ => ("null".into(), None, 0.0, 0),
                    };
                    prop_types.push(ptype);
                    value_strings.push(vstr);
                    value_reals.push(vreal);
                    value_ints.push(vint);
                }
            }
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("EntityId",    DataType::Int64,   false),
        Field::new("PsetName",    DataType::Utf8,    true),
        Field::new("PsetGlobalId", DataType::Utf8,   true),
        Field::new("PropName",    DataType::Utf8,    true),
        Field::new("PropType",    DataType::Utf8,    false),
        Field::new("ValueString", DataType::Utf8,    true),
        Field::new("ValueReal",   DataType::Float64, false),
        Field::new("ValueInt",    DataType::Int32,   false),
    ]));

    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(Int64Array::from(entity_ids)),
            Arc::new(StringArray::from(pset_names.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(pset_guids.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(prop_names.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(prop_types.iter().map(String::as_str).collect::<Vec<_>>())),
            Arc::new(StringArray::from(value_strings.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(value_reals)),
            Arc::new(Int32Array::from(value_ints)),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

fn write_quantities_table(model: &IfcModel, _step: &StepFile, props: &WriterProperties) -> Result<Vec<u8>, String> {
    use ifc_step::StepValue;

    let mut entity_ids: Vec<i64> = Vec::new();
    let mut qset_names: Vec<Option<String>> = Vec::new();
    let mut qty_names: Vec<Option<String>> = Vec::new();
    let mut qty_types: Vec<String> = Vec::new();
    let mut values: Vec<f64> = Vec::new();

    for (eqset_id, eqset) in &model.element_quantities {
        let linked_elements: Vec<u64> = model.rel_defines_by_properties
            .iter()
            .filter(|r| r.relating_property_definition == *eqset_id)
            .flat_map(|r| &r.related_objects)
            .copied()
            .collect();

        for element_id in linked_elements {
            for qty_id in &eqset.quantities {
                if let Some(qty) = model.physical_quantities.get(qty_id) {
                    let v = qty.value.as_ref().and_then(|v| match v {
                        StepValue::Real(r) => Some(*r),
                        StepValue::Int(i) => Some(*i as f64),
                        StepValue::Typed { value, .. } => match value.as_ref() {
                            StepValue::Real(r) => Some(*r),
                            StepValue::Int(i) => Some(*i as f64),
                            _ => None,
                        },
                        _ => None,
                    }).unwrap_or(0.0);

                    entity_ids.push(element_id as i64);
                    qset_names.push(eqset.name.as_ref().map(|s| s.to_string()));
                    qty_names.push(Some(qty.name.to_string()));
                    qty_types.push(qty.entity_name.to_string());
                    values.push(v);
                }
            }
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("EntityId",     DataType::Int64,   false),
        Field::new("QsetName",     DataType::Utf8,    true),
        Field::new("QuantityName", DataType::Utf8,    true),
        Field::new("QuantityType", DataType::Utf8,    false),
        Field::new("Value",        DataType::Float64, false),
    ]));

    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(Int64Array::from(entity_ids)),
            Arc::new(StringArray::from(qset_names.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(qty_names.iter().map(|o| o.as_deref()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(qty_types.iter().map(String::as_str).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(values)),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

fn write_relationships_table(model: &IfcModel, props: &WriterProperties) -> Result<Vec<u8>, String> {
    let mut source_ids: Vec<i64> = Vec::new();
    let mut target_ids: Vec<i64> = Vec::new();
    let mut rel_types: Vec<&str> = Vec::new();
    let mut rel_ids: Vec<i64> = Vec::new();

    // IsDecomposedBy / Decomposes
    for rel in &model.rel_aggregates {
        for &child in &rel.children {
            source_ids.push(rel.parent as i64);
            target_ids.push(child as i64);
            rel_types.push("IsDecomposedBy");
            rel_ids.push(0);
        }
    }
    // ContainsElements / ContainedInStructure
    for rel in &model.rel_contained {
        for &elem in &rel.elements {
            source_ids.push(rel.structure as i64);
            target_ids.push(elem as i64);
            rel_types.push("ContainsElements");
            rel_ids.push(0);
        }
    }
    // IsDefinedBy (property sets → elements)
    for rel in &model.rel_defines_by_properties {
        for &elem in &rel.related_objects {
            source_ids.push(rel.relating_property_definition as i64);
            target_ids.push(elem as i64);
            rel_types.push("IsDefinedBy");
            rel_ids.push(0);
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("SourceId", DataType::Int64, false),
        Field::new("TargetId", DataType::Int64, false),
        Field::new("RelType",  DataType::Utf8,  false),
        Field::new("RelId",    DataType::Int64, false),
    ]));

    write_parquet(schema.clone(), props, |w| {
        let b = RecordBatch::try_new(schema, vec![
            Arc::new(Int64Array::from(source_ids)),
            Arc::new(Int64Array::from(target_ids)),
            Arc::new(StringArray::from(rel_types)),
            Arc::new(Int64Array::from(rel_ids)),
        ]).map_err(|e| e.to_string())?;
        w.write(&b).map_err(|e| e.to_string())
    })
}

