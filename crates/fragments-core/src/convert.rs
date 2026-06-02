use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;

use flatbuffers::{FlatBufferBuilder, WIPOffset};
use flate2::{write::ZlibEncoder, Compression};
use fragments_schema::*;
use ifc_model::IfcModel;
use ifc_step::{StepFile, StepValue};
use serde_json::json;
use thiserror::Error;

use crate::shell_processor::get_shell_data;
use crate::step::{entity_name, geometry_instances_for_product, product_world_transform, ShellGeometry};

#[derive(Debug, Clone)]
pub struct FragmentsConfig {
    pub compress: bool,
}

impl Default for FragmentsConfig {
    fn default() -> Self {
        Self { compress: true }
    }
}

#[derive(Debug, Clone)]
pub struct FragmentsBytes {
    pub raw: Vec<u8>,
    pub compressed: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum FragmentsError {
    #[error("fragments payload exceeds u32 bounds")]
    IntegerOverflow,
    #[error("compression failed: {0}")]
    Compression(String),
}

pub fn convert_step_to_fragments(
    model: &IfcModel,
    step: &StepFile,
    config: &FragmentsConfig,
) -> Result<FragmentsBytes, FragmentsError> {
    let mut builder = FlatBufferBuilder::new();

    let entity_ids = collect_serialized_entity_ids(model, step);
    let local_ids: Vec<_> = entity_ids
        .iter()
        .copied()
        .map(to_u32)
        .collect::<Result<_, _>>()?;
    let categories_offsets: Vec<_> = entity_ids
        .iter()
        .map(|id| builder.create_string(entity_name(step, *id).unwrap_or("UNKNOWN")))
        .collect();
    let categories = builder.create_vector(&categories_offsets);
    let local_ids_vector = builder.create_vector(&local_ids);

    let mut guid_offsets = Vec::new();
    let mut guid_items = Vec::new();
    for id in &entity_ids {
        if let Some(guid) = entity_guid(model, step, *id) {
            guid_offsets.push(builder.create_string(guid));
            guid_items.push(to_u32(*id)?);
        }
    }
    let guids = builder.create_vector(&guid_offsets);
    let guids_items = builder.create_vector(&guid_items);

    let attributes = {
        let mut offsets = Vec::new();
        for id in &entity_ids {
            let values = collect_entity_attributes(model, step, *id);
            offsets.push(build_attribute(&mut builder, &values));
        }
        builder.create_vector(&offsets)
    };

    let relations = {
        let relation_map = build_relations(&mut builder, model, step, &entity_ids);

        let mut ids = Vec::new();
        let mut rel_offsets = Vec::new();
        for (id, defs) in relation_map {
            ids.push(id as i32);
            let defs_vec = builder.create_vector(&defs);
            rel_offsets.push(Relation::create(&mut builder, &RelationArgs { data: Some(defs_vec) }));
        }
        (builder.create_vector(&rel_offsets), builder.create_vector(&ids))
    };

    // Match oracle's classes.elements: include IFCSPACE from spatial nodes,
    // exclude IFCOPENINGELEMENT (commented out in upstream classes.ts).
    let mut geometry_entity_ids_set: BTreeSet<u64> = model
        .elements
        .keys()
        .copied()
        .filter(|id| !step.entities.get(id)
            .map(|e| e.entity_name == "IFCOPENINGELEMENT")
            .unwrap_or(false))
        .collect();
    for id in model.spatial_nodes.keys().copied() {
        if let Some(e) = step.entities.get(&id) {
            if e.entity_name == "IFCSPACE" {
                geometry_entity_ids_set.insert(id);
            }
        }
    }
    let geometry_entity_ids: Vec<u64> = geometry_entity_ids_set.into_iter().collect();
    let geometry_seed = step.entities.keys().max().copied().unwrap_or(0).saturating_add(1);
    let (meshes, next_local_id_after_meshes) =
        build_meshes(&mut builder, step, &geometry_entity_ids, geometry_seed)?;
    let spatial_structure = build_spatial_structure(&mut builder, model, step)?;

    let metadata = builder.create_string(&build_metadata(step));
    let root_guid = builder.create_string(
        model.spatial_nodes
            .values()
            .find(|node| step.entities.get(&node.id).map(|e| e.entity_name == "IFCPROJECT").unwrap_or(false))
            .map(|node| node.guid.as_str())
            .or_else(|| entity_ids.iter().find_map(|id| entity_guid(model, step, *id)))
            .unwrap_or("ifc2lbd-neo"),
    );

    let model_offset = Model::create(
        &mut builder,
        &ModelArgs {
            metadata: Some(metadata),
            guids: Some(guids),
            guids_items: Some(guids_items),
            max_local_id: to_u32(next_local_id_after_meshes.saturating_add(1))?,
            local_ids: Some(local_ids_vector),
            categories: Some(categories),
            meshes: Some(meshes),
            attributes: Some(attributes),
            relations: Some(relations.0),
            relations_items: Some(relations.1),
            guid: Some(root_guid),
            spatial_structure,
            unique_attributes: None,
            relation_names: None,
            indexes: None,
        },
    );
    finish_model_buffer(&mut builder, model_offset);

    let raw = builder.finished_data().to_vec();
    let compressed = if config.compress {
        compress(&raw)?
    } else {
        raw.clone()
    };

    Ok(FragmentsBytes { raw, compressed })
}

fn build_meshes<'a>(
    builder: &mut FlatBufferBuilder<'a>,
    step: &StepFile,
    sorted_elements: &[u64],
    mut next_local_id: u64,
) -> Result<(WIPOffset<Meshes<'a>>, u64), FragmentsError> {
    // Pre-build a map from geometry item ID → RGBA color via IFCSTYLEDITEM chain.
    let item_colors = build_item_color_map(step);

    // Shell dedup matches oracle's dual strategy:
    // 1. _previousGeometriesIDs: item_id → shell index (same entity → instant dedup)
    // 2. _previousGeometries: content hash → shell index (different entities, same geometry)
    let mut shell_dedup_by_id: HashMap<u64, usize> = HashMap::new();
    let mut shell_dedup_by_hash: HashMap<u64, usize> = HashMap::new();
    // Local transform dedup: 9-component bytes → index into local_transforms_data.
    // Index 0 is always the identity (matches oracle: first onLocalTransformLoaded is no-transform).
    // Sample.local_transform = 0 → identity; > 0 → that index in local_transforms.
    let mut lt_dedup: HashMap<[u32; 9], u32> = HashMap::new();
    // Material dedup: RGBA bytes → 0-indexed ID
    let mut material_dedup: HashMap<[u8; 4], u32> = HashMap::new();

    let mut shells_data: Vec<ShellGeometry> = Vec::new();
    let mut local_transforms_data: Vec<[f32; 9]> = Vec::new();
    let mut materials_data: Vec<[u8; 4]> = Vec::new();

    let mut representations: Vec<Representation> = Vec::new();
    let mut samples: Vec<Sample> = Vec::new();
    let mut global_transforms: Vec<Transform> = Vec::new();
    let mut meshes_items: Vec<u32> = Vec::new();

    let mut sample_ids: Vec<u32> = Vec::new();
    let mut representation_ids: Vec<u32> = Vec::new();
    let mut material_ids: Vec<u32> = Vec::new();
    let mut local_transform_ids: Vec<u32> = Vec::new();
    let mut global_transform_ids: Vec<u32> = Vec::new();

    // Identity transform is always index 0 (oracle: "First local transform is the no-transform").
    // Sample.local_transform = 0 means "use local_transforms[0] = identity".
    let identity_key = transform_key(&crate::step::Affine3::identity());
    lt_dedup.insert(identity_key, 0);
    local_transforms_data.push([0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    local_transform_ids.push(next_local_id as u32);
    next_local_id = next_local_id.saturating_add(1);

    let mut item_counter = 0u32;

    for element_id in sorted_elements {
        let instances = geometry_instances_for_product(step, *element_id);
        if instances.is_empty() {
            continue;
        }

        // Global transform = element world placement × first geometry's item transform.
        // Oracle: elementTransform = mesh.geometries.get(0).flatTransformation
        //       = element placement × first item placement
        let world = product_world_transform(step, *element_id);
        let first_item_transform = &instances[0].local_transform;
        let element_transform = world.mul(first_item_transform); // full world matrix of first geom

        let translation = element_transform.translation();
        let (x_axis, y_axis) = element_transform.axes();
        global_transforms.push(Transform::new(
            &DoubleVector::new(translation[0], translation[1], translation[2]),
            &FloatVector::new(x_axis[0], x_axis[1], x_axis[2]),
            &FloatVector::new(y_axis[0], y_axis[1], y_axis[2]),
        ));
        global_transform_ids.push(to_u32(*element_id)?);
        meshes_items.push(item_counter);

        let first_item_inv = first_item_transform.inverse();

        for (geom_index, instance) in instances.iter().enumerate() {
            // --- Shell dedup: ID first (oracle: _previousGeometriesIDs), then geometric hash ---
            let repr_idx = if let Some(&existing) = shell_dedup_by_id.get(&instance.item_id) {
                existing as u32
            } else {
                let shell_hash = hash_shell(&instance.shell);
                if let Some(&existing) = shell_dedup_by_hash.get(&shell_hash) {
                    shell_dedup_by_id.insert(instance.item_id, existing);
                    existing as u32
                } else {
                    let idx = shells_data.len();
                    shell_dedup_by_id.insert(instance.item_id, idx);
                    shell_dedup_by_hash.insert(shell_hash, idx);

                    let (bbox_min, bbox_max) = instance.shell.bbox();
                    representations.push(Representation::new(
                        idx as u32,
                        &BoundingBox::new(
                            &FloatVector::new(bbox_min[0], bbox_min[1], bbox_min[2]),
                            &FloatVector::new(bbox_max[0], bbox_max[1], bbox_max[2]),
                        ),
                        RepresentationClass::SHELL,
                    ));
                    representation_ids.push(next_local_id as u32);
                    next_local_id = next_local_id.saturating_add(1);
                    shells_data.push(instance.shell.clone());
                    idx as u32
                }
            };

            // --- Material ---
            let color = item_colors.get(&instance.item_id).copied()
                .unwrap_or([200u8, 200, 200, 255]);
            let mat_idx = *material_dedup.entry(color).or_insert_with(|| {
                let idx = materials_data.len() as u32;
                materials_data.push(color);
                material_ids.push(next_local_id as u32);
                next_local_id = next_local_id.saturating_add(1);
                idx
            });

            // --- Local transform ---
            // First geometry per element OR identity transform → 0 (no transform).
            // Non-identity subsequent geometries get a 1-indexed ID.
            // Local transform (oracle algorithm from getLocalTransform):
            //   localTransform = firstItemTransform^(-1) × thisItemTransform
            // First geometry per element → identity (index 0, the no-transform).
            let lt_id = if geom_index == 0 {
                0u32
            } else {
                let relative = first_item_inv.mul(&instance.local_transform);
                if relative.is_identity() {
                    0u32
                } else {
                    let key = transform_key(&relative);
                    *lt_dedup.entry(key).or_insert_with(|| {
                        let idx = local_transforms_data.len() as u32;
                        let pos = relative.translation();
                        let (lx, ly) = relative.axes();
                        local_transforms_data.push([
                            pos[0] as f32, pos[1] as f32, pos[2] as f32,
                            lx[0], lx[1], lx[2],
                            ly[0], ly[1], ly[2],
                        ]);
                        local_transform_ids.push(next_local_id as u32);
                        next_local_id = next_local_id.saturating_add(1);
                        idx
                    })
                }
            };

            samples.push(Sample::new(item_counter, mat_idx, repr_idx, lt_id));
            sample_ids.push(next_local_id as u32);
            next_local_id = next_local_id.saturating_add(1);
        }

        item_counter += 1;
    }

    // Build FlatBuffer shell offsets using get_shell_data (oracle's profile-based format)
    let mut shell_offsets: Vec<WIPOffset<Shell>> = Vec::with_capacity(shells_data.len());
    for shell in &shells_data {
        let (positions, normals, triangles) = shell.to_triangulated();

        // Run oracle's getShellData to get profiles/holes/points
        let shell_data = get_shell_data(&positions, &normals, &triangles);

        let is_big = shell_data.points.len() > 65000;

        let point_vec: Vec<_> = shell_data.points.iter()
            .map(|p| FloatVector::new(p[0], p[1], p[2]))
            .collect();
        let points_offset = builder.create_vector(&point_vec);

        // Sort profiles by key for deterministic output
        let mut sorted_profile_keys: Vec<usize> = shell_data.profiles.keys().copied().collect();
        sorted_profile_keys.sort_unstable();

        let face_ids_offset = builder.create_vector(&shell_data.profiles_face_ids);

        if is_big {
            let mut big_profile_offsets = Vec::new();
            for key in &sorted_profile_keys {
                let profile = &shell_data.profiles[key];
                let indices: Vec<u32> = profile.iter().copied().collect();
                let indices_offset = builder.create_vector(&indices);
                big_profile_offsets.push(BigShellProfile::create(builder, &BigShellProfileArgs {
                    indices: Some(indices_offset),
                }));
            }
            // Holes (big)
            let mut big_hole_offsets = Vec::new();
            let mut sorted_hole_keys: Vec<usize> = shell_data.holes.keys().copied().collect();
            sorted_hole_keys.sort_unstable();
            for hole_key in &sorted_hole_keys {
                for hole in &shell_data.holes[hole_key] {
                    let indices: Vec<u32> = hole.iter().copied().collect();
                    let indices_offset = builder.create_vector(&indices);
                    big_hole_offsets.push(BigShellHole::create(builder, &BigShellHoleArgs {
                        indices: Some(indices_offset),
                        profile_id: *hole_key as u16,
                    }));
                }
            }
            let big_profiles_offset = builder.create_vector(&big_profile_offsets);
            let big_holes_offset = builder.create_vector(&big_hole_offsets);
            let profiles_offset = builder.create_vector::<WIPOffset<ShellProfile>>(&[]);
            let holes_offset = builder.create_vector::<WIPOffset<ShellHole>>(&[]);
            shell_offsets.push(Shell::create(builder, &ShellArgs {
                profiles: Some(profiles_offset),
                holes: Some(holes_offset),
                points: Some(points_offset),
                big_profiles: Some(big_profiles_offset),
                big_holes: Some(big_holes_offset),
                type_: ShellType::BIG,
                profiles_face_ids: Some(face_ids_offset),
            }));
        } else {
            let mut profile_offsets = Vec::new();
            for key in &sorted_profile_keys {
                let profile = &shell_data.profiles[key];
                let indices: Vec<u16> = profile.iter()
                    .filter_map(|&i| u16::try_from(i).ok())
                    .collect();
                let indices_offset = builder.create_vector(&indices);
                profile_offsets.push(ShellProfile::create(builder, &ShellProfileArgs {
                    indices: Some(indices_offset),
                }));
            }
            // Holes
            let mut hole_offsets = Vec::new();
            let mut sorted_hole_keys: Vec<usize> = shell_data.holes.keys().copied().collect();
            sorted_hole_keys.sort_unstable();
            for hole_key in &sorted_hole_keys {
                for hole in &shell_data.holes[hole_key] {
                    let indices: Vec<u16> = hole.iter()
                        .filter_map(|&i| u16::try_from(i).ok())
                        .collect();
                    let indices_offset = builder.create_vector(&indices);
                    hole_offsets.push(ShellHole::create(builder, &ShellHoleArgs {
                        indices: Some(indices_offset),
                        profile_id: *hole_key as u16,
                    }));
                }
            }
            let profiles_offset = builder.create_vector(&profile_offsets);
            let holes_offset = builder.create_vector(&hole_offsets);
            let big_profiles_offset = builder.create_vector::<WIPOffset<BigShellProfile>>(&[]);
            let big_holes_offset = builder.create_vector::<WIPOffset<BigShellHole>>(&[]);
            shell_offsets.push(Shell::create(builder, &ShellArgs {
                profiles: Some(profiles_offset),
                holes: Some(holes_offset),
                points: Some(points_offset),
                big_profiles: Some(big_profiles_offset),
                big_holes: Some(big_holes_offset),
                type_: ShellType::NONE,
                profiles_face_ids: Some(face_ids_offset),
            }));
        }
    }

    // Build FlatBuffer material structs
    let material_structs: Vec<Material> = materials_data.iter().map(|[r, g, b, a]| {
        Material::new(*r, *g, *b, *a, RenderedFaces::ONE, Stroke::DEFAULT)
    }).collect();

    // Build FlatBuffer local transform structs (non-identity only)
    let lt_structs: Vec<Transform> = local_transforms_data.iter().map(|d| {
        Transform::new(
            &DoubleVector::new(d[0] as f64, d[1] as f64, d[2] as f64),
            &FloatVector::new(d[3], d[4], d[5]),
            &FloatVector::new(d[6], d[7], d[8]),
        )
    }).collect();

    let shells_offset = builder.create_vector(&shell_offsets);
    let representations_offset = builder.create_vector(&representations);
    let samples_offset = builder.create_vector(&samples);
    let materials_offset = builder.create_vector(&material_structs);
    let local_transforms_offset = builder.create_vector(&lt_structs);
    let global_transforms_offset = builder.create_vector(&global_transforms);
    let meshes_items_offset = builder.create_vector(&meshes_items);
    let sample_ids_offset = builder.create_vector(&sample_ids);
    let representation_ids_offset = builder.create_vector(&representation_ids);
    let material_ids_offset = builder.create_vector(&material_ids);
    let local_transform_ids_offset = builder.create_vector(&local_transform_ids);
    let global_transform_ids_offset = builder.create_vector(&global_transform_ids);
    let empty_circle_extrusions = builder.create_vector::<WIPOffset<CircleExtrusion>>(&[]);
    let coordinates = Transform::new(
        &DoubleVector::new(0.0, 0.0, 0.0),
        &FloatVector::new(1.0, 0.0, 0.0),
        &FloatVector::new(0.0, 1.0, 0.0),
    );

    Ok((
        Meshes::create(builder, &MeshesArgs {
            coordinates: Some(&coordinates),
            meshes_items: Some(meshes_items_offset),
            samples: Some(samples_offset),
            representations: Some(representations_offset),
            materials: Some(materials_offset),
            circle_extrusions: Some(empty_circle_extrusions),
            shells: Some(shells_offset),
            local_transforms: Some(local_transforms_offset),
            global_transforms: Some(global_transforms_offset),
            material_ids: Some(material_ids_offset),
            representation_ids: Some(representation_ids_offset),
            sample_ids: Some(sample_ids_offset),
            local_transform_ids: Some(local_transform_ids_offset),
            global_transform_ids: Some(global_transform_ids_offset),
        }),
        next_local_id,
    ))
}

/// Geometry deduplication hash matching oracle's metric-based approach from loadShellGeometry.
/// Oracle hash string: "${vertexCount}-${triangleCount}-${areaSum}-${biggestArea}-${volume}-${cx}-${cy}-${cz}-${x1}-${y1}-${z1}"
/// All float values rounded to precision p = 10000.
fn hash_shell(shell: &ShellGeometry) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let precision = 10000.0f64;
    let round = |v: f64| -> i64 { (v * precision).round() as i64 };

    let vertex_count = shell.points.len();

    // Triangulate faces into triangles for metric computation
    let mut triangles: Vec<[usize; 3]> = Vec::new();
    for face in &shell.faces {
        for i in 1..face.len().saturating_sub(1) {
            triangles.push([face[0] as usize, face[i] as usize, face[i + 1] as usize]);
        }
    }
    let triangle_count = triangles.len();

    let p = &shell.points;
    let mut area_sum = 0.0f64;
    let mut biggest_area = 0.0f64;
    let mut volume = 0.0f64;
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    let mut cz = 0.0f64;

    for &[i1, i2, i3] in &triangles {
        if i1 >= p.len() || i2 >= p.len() || i3 >= p.len() { continue; }
        let (a, b, c) = (p[i1], p[i2], p[i3]);
        let (a, b, c) = (
            [a[0] as f64, a[1] as f64, a[2] as f64],
            [b[0] as f64, b[1] as f64, b[2] as f64],
            [c[0] as f64, c[1] as f64, c[2] as f64],
        );
        // Cross product (b-a) × (c-a)
        let ab = [b[0]-a[0], b[1]-a[1], b[2]-a[2]];
        let ac = [c[0]-a[0], c[1]-a[1], c[2]-a[2]];
        let cross = [
            ab[1]*ac[2] - ab[2]*ac[1],
            ab[2]*ac[0] - ab[0]*ac[2],
            ab[0]*ac[1] - ab[1]*ac[0],
        ];
        let area = (cross[0]*cross[0] + cross[1]*cross[1] + cross[2]*cross[2]).sqrt() * 0.5;
        area_sum += area;
        if area > biggest_area { biggest_area = area; }

        // Signed volume contribution (shoelace in 3D)
        volume += a[0] * (b[1]*c[2] - c[1]*b[2])
                - a[1] * (b[0]*c[2] - c[0]*b[2])
                + a[2] * (b[0]*c[1] - c[0]*b[1]);

        // Centroid accumulation
        cx += a[0] + b[0] + c[0];
        cy += a[1] + b[1] + c[1];
        cz += a[2] + b[2] + c[2];
    }

    let total_verts = (triangle_count * 3) as f64;
    if total_verts > 0.0 {
        cx /= total_verts;
        cy /= total_verts;
        cz /= total_verts;
    }

    // First 3 vertices (may be fewer)
    let x1 = p.first().map(|v| v[0] as f64).unwrap_or(0.0);
    let y1 = p.first().map(|v| v[1] as f64).unwrap_or(0.0);
    let z1 = p.first().map(|v| v[2] as f64).unwrap_or(0.0);

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

/// 9-component key for local transform deduplication.
fn transform_key(t: &crate::step::Affine3) -> [u32; 9] {
    let pos = t.translation();
    let (x, y) = t.axes();
    [
        (pos[0] as f32).to_bits(), (pos[1] as f32).to_bits(), (pos[2] as f32).to_bits(),
        x[0].to_bits(), x[1].to_bits(), x[2].to_bits(),
        y[0].to_bits(), y[1].to_bits(), y[2].to_bits(),
    ]
}

/// Build a map from geometry item ID → RGBA color via IFCSTYLEDITEM chain.
fn build_item_color_map(step: &StepFile) -> HashMap<u64, [u8; 4]> {
    let mut map = HashMap::new();

    for entity in step.entities.values() {
        if entity.entity_name != "IFCSTYLEDITEM" { continue; }
        let Some(item_id) = entity.args.first().and_then(StepValue::as_ref) else { continue; };
        let Some(styles) = entity.args.get(1).and_then(StepValue::as_list) else { continue; };
        for style_ref in styles {
            let Some(style_id) = style_ref.as_ref() else { continue; };
            if let Some(rgba) = resolve_style_color(step, style_id) {
                map.insert(item_id, rgba);
                break;
            }
        }
    }
    map
}

fn resolve_style_color(step: &StepFile, style_id: u64) -> Option<[u8; 4]> {
    let entity = step.entities.get(&style_id)?;
    match entity.entity_name.as_str() {
        "IFCPRESENTATIONSTYLEASSIGNMENT" | "IFCSTYLEASSIGNMENT" => {
            let styles = entity.args.first().and_then(StepValue::as_list)?;
            for s in styles {
                let id = s.as_ref()?;
                if let Some(c) = resolve_style_color(step, id) { return Some(c); }
            }
            None
        }
        "IFCSURFACESTYLE" => {
            // arg[2] = Styles (list of rendering styles)
            let styles = entity.args.get(2).and_then(StepValue::as_list)?;
            for s in styles {
                let id = s.as_ref()?;
                if let Some(c) = resolve_style_color(step, id) { return Some(c); }
            }
            None
        }
        "IFCSURFACESTYLERENDERING" => {
            let colour_id = entity.args.first().and_then(StepValue::as_ref)?;
            let transparency = entity.args.get(1)
                .and_then(|v| match v { StepValue::Real(r) => Some(*r), StepValue::Typed { value, .. } => if let StepValue::Real(r) = value.as_ref() { Some(*r) } else { None }, _ => None })
                .unwrap_or(0.0);
            let colour = step.entities.get(&colour_id)?;
            if colour.entity_name != "IFCCOLOURRGB" { return None; }
            let r = colour.args.get(1).and_then(real_from_step_value)?;
            let g = colour.args.get(2).and_then(real_from_step_value)?;
            let b = colour.args.get(3).and_then(real_from_step_value)?;
            let a = ((1.0 - transparency.clamp(0.0, 1.0)) * 255.0) as u8;
            Some([
                (r.clamp(0.0, 1.0) * 255.0) as u8,
                (g.clamp(0.0, 1.0) * 255.0) as u8,
                (b.clamp(0.0, 1.0) * 255.0) as u8,
                a,
            ])
        }
        _ => None,
    }
}

fn real_from_step_value(v: &StepValue) -> Option<f64> {
    match v {
        StepValue::Real(r) => Some(*r),
        StepValue::Int(i) => Some(*i as f64),
        StepValue::Typed { value, .. } => real_from_step_value(value),
        _ => None,
    }
}

fn build_spatial_structure<'a>(
    builder: &mut FlatBufferBuilder<'a>,
    model: &IfcModel,
    step: &StepFile,
) -> Result<Option<WIPOffset<SpatialStructure<'a>>>, FragmentsError> {
    let mut spatial_children: HashMap<u64, Vec<u64>> = HashMap::new();
    for rel in &model.rel_aggregates {
        spatial_children.entry(rel.parent).or_default().extend(rel.children.iter().copied());
    }
    let mut contained: HashMap<u64, Vec<u64>> = HashMap::new();
    for rel in &model.rel_contained {
        contained.entry(rel.structure).or_default().extend(rel.elements.iter().copied());
    }

    let child_set: BTreeSet<_> = spatial_children.values().flat_map(|children| children.iter().copied()).collect();
    let roots: Vec<_> = model
        .spatial_nodes
        .keys()
        .copied()
        .filter(|id| !child_set.contains(id))
        .collect();
    let Some(root_id) = roots.first().copied() else {
        return Ok(None);
    };

    fn build_node<'a>(
        builder: &mut FlatBufferBuilder<'a>,
        step: &StepFile,
        model: &IfcModel,
        spatial_children: &HashMap<u64, Vec<u64>>,
        contained: &HashMap<u64, Vec<u64>>,
        id: u64,
    ) -> Result<WIPOffset<SpatialStructure<'a>>, FragmentsError> {
        let mut child_offsets = Vec::new();
        if let Some(children) = spatial_children.get(&id) {
            for child in children {
                child_offsets.push(build_node(builder, step, model, spatial_children, contained, *child)?);
            }
        }
        if let Some(elements) = contained.get(&id) {
            for element_id in elements {
                let category = builder.create_string(entity_name(step, *element_id).unwrap_or("UNKNOWN"));
                child_offsets.push(SpatialStructure::create(
                    builder,
                    &SpatialStructureArgs {
                        local_id: Some(to_u32(*element_id)?),
                        category: Some(category),
                        children: None,
                    },
                ));
            }
        }

        let children = if child_offsets.is_empty() {
            None
        } else {
            Some(builder.create_vector(&child_offsets))
        };
        let category = builder.create_string(entity_name(step, id).unwrap_or("UNKNOWN"));
        Ok(SpatialStructure::create(
            builder,
            &SpatialStructureArgs {
                local_id: Some(to_u32(id)?),
                category: Some(category),
                children,
            },
        ))
    }

    build_node(builder, step, model, &spatial_children, &contained, root_id).map(Some)
}

fn build_attribute<'a>(builder: &mut FlatBufferBuilder<'a>, values: &[String]) -> WIPOffset<Attribute<'a>> {
    let data: Vec<_> = values
        .iter()
        .map(|value| builder.create_string(value))
        .collect();
    let vec = builder.create_vector(&data);
    Attribute::create(builder, &AttributeArgs { data: Some(vec) })
}

fn build_metadata(step: &StepFile) -> String {
    format!(
        "{{\"schema\":{},\"names\":{},\"descriptions\":{},\"crs\":null}}",
        serde_json::to_string(&step.header.schema_raw).unwrap_or_else(|_| "\"IFC4\"".to_string()),
        serde_json::to_string(&step.header.file_name).unwrap_or_else(|_| "[]".to_string()),
        serde_json::to_string(&step.header.description).unwrap_or_else(|_| "[]".to_string())
    )
}

fn collect_entity_attributes(model: &IfcModel, step: &StepFile, id: u64) -> Vec<String> {
    let Some(entity) = step.entities.get(&id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, value) in entity.args.iter().enumerate() {
        let attr_name = attribute_name(entity.entity_name.as_str(), index);
        if should_exclude_attribute(attr_name) {
            continue;
        }
        if attr_name == "GlobalId" {
            continue;
        }
        match value {
            StepValue::Null | StepValue::Derived => {}
            StepValue::Ref(_) => {}
            StepValue::List(items) if items.iter().all(|item| item.as_ref().is_some()) => {}
            StepValue::List(items) => {
                if let Some(encoded) = encode_scalar_list_attribute(attr_name, items) {
                    out.push(encoded);
                }
            }
            _ => {
                if let Some((value, kind)) = scalar_value_and_type(value) {
                    out.push(json!([attr_name, value, kind]).to_string());
                }
            }
        }
    }

    if entity.entity_name == "IFCSITE" {
        out.push(json!(["RefElevation", json_number(absolute_elevation(step, id)), "IFCLENGTHMEASURE"]).to_string());
    }
    if entity.entity_name == "IFCBUILDINGSTOREY" {
        out.push(json!(["Elevation", json_number(absolute_elevation(step, id)), "IFCLENGTHMEASURE"]).to_string());
    }

    if !model.spatial_nodes.contains_key(&id)
        && !model.elements.contains_key(&id)
        && entity.entity_name == "IFCUNITASSIGNMENT"
    {
        return out;
    }
    out
}

fn build_relations<'a>(
    builder: &mut FlatBufferBuilder<'a>,
    _model: &IfcModel,
    step: &StepFile,
    entity_ids: &[u64],
) -> BTreeMap<u32, Vec<WIPOffset<&'a str>>> {
    let mut relation_values: BTreeMap<u64, Vec<String>> = BTreeMap::new();

    // Pass 1: inline refs from every serialized entity (matching oracle's serializeAttributes).
    // Scalar refs → single-element relation; all-ref lists → multi-id relation.
    // Skip GlobalId (excluded explicitly) and attributesToExclude.
    for id in entity_ids {
        let Some(entity) = step.entities.get(id) else { continue; };
        for (index, value) in entity.args.iter().enumerate() {
            let attr_name = attribute_name(entity.entity_name.as_str(), index);
            if attr_name == "GlobalId" || should_exclude_attribute(attr_name) {
                continue;
            }
            match value {
                StepValue::Ref(target) => {
                    relation_values
                        .entry(*id)
                        .or_default()
                        .push(json!([attr_name, *target]).to_string());
                }
                StepValue::List(items) if items.iter().all(|item| item.as_ref().is_some()) => {
                    let ids: Vec<u64> = items.iter().filter_map(|v| v.as_ref()).collect();
                    if !ids.is_empty() {
                        let mut payload = vec![json!(attr_name)];
                        payload.extend(ids.iter().map(|id| json!(id)));
                        relation_values.entry(*id).or_default().push(json!(payload).to_string());
                    }
                }
                StepValue::Derived if attr_name == "Dimensions" => {
                    relation_values.entry(*id).or_default()
                        .push(json!(["Dimensions", 0]).to_string());
                }
                _ => {}
            }
        }
    }

    // Pass 2: semantic bidirectional relations from the 5 IFCREL* types, matching the
    // oracle's `relations` map in index.ts exactly:
    //   IFCRELAGGREGATES              → IsDecomposedBy / Decomposes
    //   IFCRELDEFINESBYPROPERTIES     → DefinesOccurrence / IsDefinedBy
    //   IFCRELDEFINESBYTYPE           → ObjectTypeOf / IsTypedBy
    //   IFCRELASSOCIATESMATERIAL      → AssociatedTo / HasAssociations
    //   IFCRELCONTAINEDINSPATIALSTRUCTURE → ContainsElements / ContainedInStructure
    //
    // Each IFCREL* entity has arg[4] and arg[5] as the two sides of the relationship.
    // Which side is "relating" vs "related" varies by type.
    const REL_TYPES: &[(&str, usize, &str, usize, &str)] = &[
        // (entity_name, relating_arg, for_relating, related_arg, for_related)
        ("IFCRELAGGREGATES",                   4, "IsDecomposedBy",   5, "Decomposes"),
        ("IFCRELDEFINESBYPROPERTIES",           5, "DefinesOccurrence", 4, "IsDefinedBy"),
        ("IFCRELDEFINESBYTYPE",                 5, "ObjectTypeOf",    4, "IsTypedBy"),
        ("IFCRELASSOCIATESMATERIAL",            5, "AssociatedTo",    4, "HasAssociations"),
        ("IFCRELCONTAINEDINSPATIALSTRUCTURE",   5, "ContainsElements", 4, "ContainedInStructure"),
    ];

    for &(entity_name, relating_arg, for_relating, related_arg, for_related) in REL_TYPES {
        for entity in step.entities.values() {
            if entity.entity_name != entity_name { continue; }

            // relating side: always a single ref
            let Some(relating_id) = entity.args.get(relating_arg).and_then(StepValue::as_ref) else {
                continue;
            };

            // related side: either a list of refs or a single ref
            let related_ids: Vec<u64> = match entity.args.get(related_arg) {
                Some(StepValue::List(items)) => items.iter().filter_map(|v| v.as_ref()).collect(),
                Some(StepValue::Ref(id)) => vec![*id],
                _ => continue,
            };
            if related_ids.is_empty() { continue; }

            // relating entity → for_relating → [all related ids]
            let mut relating_payload = vec![json!(for_relating)];
            relating_payload.extend(related_ids.iter().map(|id| json!(id)));
            relation_values.entry(relating_id).or_default()
                .push(json!(relating_payload).to_string());

            // each related entity → for_related → [relating id]
            for related_id in &related_ids {
                relation_values.entry(*related_id).or_default()
                    .push(json!([for_related, relating_id]).to_string());
            }
        }
    }

    let mut relation_map = BTreeMap::new();
    for (id, defs) in relation_values {
        let offsets = defs
            .into_iter()
            .map(|value| builder.create_string(&value))
            .collect::<Vec<_>>();
        relation_map.insert(id as u32, offsets);
    }
    relation_map
}

fn collect_serialized_entity_ids(model: &IfcModel, step: &StepFile) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();

    // Do NOT unconditionally push entity #1 — it may not be IFCPROJECT.
    // IFCPROJECT/IFCSITE/IFCBUILDING/IFCBUILDINGSTOREY come in via spatial_nodes below.

    for unit_assignment in model.unit_assignments.keys().copied().collect::<BTreeSet<_>>() {
        push_unique(&mut ids, &mut seen, step, unit_assignment);
        if let Some(unit_ids) = model.unit_assignments.get(&unit_assignment).map(|a| &a.units) {
            for unit_id in unit_ids {
                push_unique(&mut ids, &mut seen, step, *unit_id);
            }
        }
    }

    let mut spatial = model.spatial_nodes.keys().copied().collect::<Vec<_>>();
    spatial.sort_by_key(|id| std::cmp::Reverse(spatial_depth(model, *id)));
    for id in spatial {
        push_unique(&mut ids, &mut seen, step, id);
    }

    let mut elements = model.elements.keys().copied().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
    elements.sort_unstable();
    for id in elements {
        // Oracle explicitly excludes IFCOPENINGELEMENT from its entity list
        // (it is commented out in classes.elements in classes.ts).
        if step.entities.get(&id).map(|e| e.entity_name == "IFCOPENINGELEMENT").unwrap_or(false) {
            continue;
        }
        push_unique(&mut ids, &mut seen, step, id);
    }

    // All remaining "semantic" entities: properties, materials, relations, type objects, styles.
    // Sorted ascending by ID to keep deterministic output.
    let mut extra: Vec<u64> = step
        .entities
        .iter()
        .filter(|(id, entity)| !seen.contains(*id) && should_serialize_entity(&entity.entity_name))
        .map(|(id, _)| *id)
        .collect();
    extra.sort_unstable();
    for id in extra {
        if step.entities.contains_key(&id) {
            seen.insert(id);
            ids.push(id);
        }
    }

    ids
}

/// Returns true for entity types that should appear in the fragments entity list
/// (local_ids / categories / attributes vectors).
///
/// Matches `ifcClasses.abstract ∪ ifcClasses.elements` from the upstream classes.ts.
/// IFCREL* types are deliberately excluded — they are only used to build the relations
/// map (see `build_semantic_relations`) and must NOT appear as entities.
fn should_serialize_entity(name: &str) -> bool {
    // Type objects — all *TYPE suffix (IFCWALLTYPE, IFCSLABTYPE, …)
    // Exclude IFCREL* types (IFCRELDEFINESBYTYPE etc.) — those are relation-only.
    if name.ends_with("TYPE") && !name.starts_with("IFCREL") { return true; }
    // classes.properties
    matches!(
        name,
        "IFCPROPERTYSET"
            | "IFCPROPERTYSINGLEVALUE"
            | "IFCELEMENTQUANTITY"
            | "IFCQUANTITYAREA"
            | "IFCQUANTITYCOUNT"
            | "IFCQUANTITYLENGTH"
            | "IFCQUANTITYNUMBER"
            | "IFCQUANTITYTIME"
            | "IFCQUANTITYVOLUME"
            | "IFCQUANTITYWEIGHT"
        // classes.materials
            | "IFCMATERIAL"
            | "IFCMATERIALLIST"
            | "IFCMATERIALCONSTITUENT"
            | "IFCMATERIALCONSTITUENTSET"
            | "IFCMATERIALLAYER"
            | "IFCMATERIALLAYERSET"
            | "IFCMATERIALLAYERSETUSAGE"
            | "IFCMATERIALPROFILE"
            | "IFCMATERIALPROFILESET"
            | "IFCMATERIALPROFILESETUSAGE"
        // classes.units (IFCUNITASSIGNMENT + IFCSIUNIT already added via model.unit_assignments)
            | "IFCNAMEDUNIT"
            | "IFCDERIVEDUNIT"
            | "IFCMONETARYUNIT"
    )
}

fn push_unique(ids: &mut Vec<u64>, seen: &mut HashSet<u64>, step: &StepFile, id: u64) {
    if step.entities.contains_key(&id) && seen.insert(id) {
        ids.push(id);
    }
}


fn spatial_depth(model: &IfcModel, id: u64) -> usize {
    let mut depth = 0usize;
    let mut current = id;
    while let Some(parent) = model
        .rel_aggregates
        .iter()
        .find(|rel| rel.children.contains(&current))
        .map(|rel| rel.parent)
    {
        depth += 1;
        current = parent;
    }
    depth
}

fn entity_guid<'a>(model: &'a IfcModel, step: &'a StepFile, id: u64) -> Option<&'a str> {
    model
        .spatial_nodes
        .get(&id)
        .map(|node| node.guid.as_str())
        .or_else(|| model.elements.get(&id).map(|node| node.guid.as_str()))
        .or_else(|| {
            step.entities.get(&id).and_then(|entity| {
                // Only extract GlobalId for IfcRoot subtypes.
                // Non-IfcRoot entities (IFCMATERIAL*, IFCQUANTITY*, IFCPROPERTYSINGLEVALUE, etc.)
                // have a Name as arg[0] which may coincidentally pass the 22-char check.
                if !entity_type_is_ifc_root(&entity.entity_name) {
                    return None;
                }
                match entity.args.first() {
                    Some(StepValue::String(s)) if is_ifc_guid(s) => Some(s.as_str()),
                    _ => None,
                }
            })
        })
}

/// Returns true for entity types that are IfcRoot subtypes in our serialization set,
/// i.e. types that carry a GlobalId as their first argument.
/// model.spatial_nodes and model.elements are already handled before this check.
fn entity_type_is_ifc_root(name: &str) -> bool {
    name == "IFCPROPERTYSET"
        || name == "IFCELEMENTQUANTITY"
        || (name.ends_with("TYPE") && !name.starts_with("IFCREL"))
}

/// IFC GlobalId (IfcGloballyUniqueId) is a 22-character base64-compressed UUID.
/// Valid characters: 0–9, A–Z, a–z, _, $
fn is_ifc_guid(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

fn should_exclude_attribute(name: &str) -> bool {
    matches!(
        name,
        "Representation" | "ObjectPlacement" | "CompositionType" | "OwnerHistory" | "Elevation" | "RefElevation"
    )
}

fn attribute_name(entity_name: &str, index: usize) -> &'static str {
    match (entity_name, index) {
        // IfcProject extra slots beyond IfcRoot
        ("IFCPROJECT", 7) => "RepresentationContexts",
        ("IFCPROJECT", 8) => "UnitsInContext",
        // Unit entities
        ("IFCUNITASSIGNMENT", 0) => "Units",
        ("IFCSIUNIT", 0) => "Dimensions",
        ("IFCSIUNIT", 1) => "UnitType",
        ("IFCSIUNIT", 3) => "Name",
        // Spatial structure nodes
        ("IFCSITE", 2) => "Name",
        ("IFCBUILDING", 2) => "Name",
        ("IFCBUILDINGSTOREY", 2) => "Name",
        ("IFCSITE", 5) => "ObjectPlacement",
        ("IFCBUILDING", 5) => "ObjectPlacement",
        ("IFCBUILDINGSTOREY", 5) => "ObjectPlacement",
        ("IFCBUILDINGSTOREY", 9) => "Elevation",
        // Property set / quantity set list fields (arg[4] is the collection)
        ("IFCPROPERTYSET", 4) => "HasProperties",
        ("IFCELEMENTQUANTITY", 5) => "Quantities",
        ("IFCMATERIALLAYERSET", 1) => "MaterialLayers",
        ("IFCMATERIALLAYERSET", 2) => "LayerSetName",
        ("IFCMATERIALPROFILESET", 1) => "MaterialProfiles",
        ("IFCMATERIALPROFILESET", 2) => "Name",
        // Properties (non-IfcRoot: no GlobalId/OwnerHistory prefix)
        ("IFCPROPERTYSINGLEVALUE", 0) => "Name",
        ("IFCPROPERTYSINGLEVALUE", 1) => "Description",
        ("IFCPROPERTYSINGLEVALUE", 2) => "NominalValue",
        ("IFCPROPERTYSINGLEVALUE", 3) => "Unit",
        // Quantities (non-IfcRoot)
        ("IFCQUANTITYLENGTH", 0) => "Name",
        ("IFCQUANTITYLENGTH", 1) => "Description",
        ("IFCQUANTITYLENGTH", 2) => "Unit",
        ("IFCQUANTITYLENGTH", 3) => "LengthValue",
        ("IFCQUANTITYLENGTH", 4) => "Formula",
        ("IFCQUANTITYAREA", 0) => "Name",
        ("IFCQUANTITYAREA", 1) => "Description",
        ("IFCQUANTITYAREA", 2) => "Unit",
        ("IFCQUANTITYAREA", 3) => "AreaValue",
        ("IFCQUANTITYAREA", 4) => "Formula",
        ("IFCQUANTITYVOLUME", 0) => "Name",
        ("IFCQUANTITYVOLUME", 1) => "Description",
        ("IFCQUANTITYVOLUME", 2) => "Unit",
        ("IFCQUANTITYVOLUME", 3) => "VolumeValue",
        ("IFCQUANTITYVOLUME", 4) => "Formula",
        ("IFCQUANTITYCOUNT", 0) => "Name",
        ("IFCQUANTITYCOUNT", 1) => "Description",
        ("IFCQUANTITYCOUNT", 2) => "Unit",
        ("IFCQUANTITYCOUNT", 3) => "CountValue",
        ("IFCQUANTITYCOUNT", 4) => "Formula",
        ("IFCQUANTITYWEIGHT", 0) => "Name",
        ("IFCQUANTITYWEIGHT", 1) => "Description",
        ("IFCQUANTITYWEIGHT", 2) => "Unit",
        ("IFCQUANTITYWEIGHT", 3) => "WeightValue",
        ("IFCQUANTITYWEIGHT", 4) => "Formula",
        ("IFCQUANTITYTIME", 0) => "Name",
        ("IFCQUANTITYTIME", 1) => "Description",
        ("IFCQUANTITYTIME", 2) => "Unit",
        ("IFCQUANTITYTIME", 3) => "TimeValue",
        ("IFCQUANTITYTIME", 4) => "Formula",
        ("IFCPHYSICALCOMPLEXQUANTITY", 0) => "Name",
        ("IFCPHYSICALCOMPLEXQUANTITY", 1) => "Description",
        ("IFCPHYSICALCOMPLEXQUANTITY", 2) => "HasQuantities",
        ("IFCPHYSICALCOMPLEXQUANTITY", 3) => "Discrimination",
        ("IFCPHYSICALCOMPLEXQUANTITY", 4) => "Quality",
        ("IFCPHYSICALCOMPLEXQUANTITY", 5) => "Usage",
        // Material entities (non-IfcRoot)
        ("IFCMATERIAL", 0) => "Name",
        ("IFCMATERIAL", 1) => "Description",
        ("IFCMATERIAL", 2) => "Category",
        ("IFCMATERIALCONSTITUENT", 0) => "Name",
        ("IFCMATERIALCONSTITUENT", 1) => "Description",
        ("IFCMATERIALCONSTITUENT", 2) => "Material",
        ("IFCMATERIALCONSTITUENT", 3) => "Fraction",
        ("IFCMATERIALCONSTITUENT", 4) => "Category",
        ("IFCMATERIALCONSTITUENTSET", 0) => "Name",
        ("IFCMATERIALCONSTITUENTSET", 1) => "Description",
        ("IFCMATERIALCONSTITUENTSET", 2) => "MaterialConstituents",
        ("IFCMATERIALLAYER", 0) => "Material",
        ("IFCMATERIALLAYER", 1) => "LayerThickness",
        ("IFCMATERIALLAYER", 2) => "IsVentilated",
        ("IFCMATERIALLAYER", 3) => "Name",
        ("IFCMATERIALLAYER", 4) => "Description",
        ("IFCMATERIALLAYER", 5) => "Category",
        ("IFCMATERIALLAYER", 6) => "Priority",
        // Presentation / style (non-IfcRoot)
        ("IFCCOLOURRGB", 0) => "Name",
        ("IFCCOLOURRGB", 1) => "Red",
        ("IFCCOLOURRGB", 2) => "Green",
        ("IFCCOLOURRGB", 3) => "Blue",
        ("IFCSURFACESTYLERENDERING", 0) => "SurfaceColour",
        ("IFCSURFACESTYLERENDERING", 1) => "Transparency",
        ("IFCSTYLEDITEM", 0) => "Item",
        ("IFCSTYLEDITEM", 1) => "Styles",
        ("IFCSTYLEDITEM", 2) => "Name",
        ("IFCSURFACESTYLE", 0) => "Name",
        ("IFCSURFACESTYLE", 1) => "Side",
        ("IFCSURFACESTYLE", 2) => "Styles",
        // Metadata entities
        ("IFCORGANIZATION", 0) => "Identifier",
        ("IFCORGANIZATION", 1) => "Name",
        ("IFCORGANIZATION", 2) => "Description",
        ("IFCAPPLICATION", 0) => "ApplicationDeveloper",
        ("IFCAPPLICATION", 1) => "Version",
        ("IFCAPPLICATION", 2) => "ApplicationFullName",
        ("IFCAPPLICATION", 3) => "ApplicationIdentifier",
        _ => default_attribute_name(index),
    }
}

fn default_attribute_name(index: usize) -> &'static str {
    const DEFAULT_NAMES: &[&str] = &[
        "GlobalId",
        "OwnerHistory",
        "Name",
        "Description",
        "ObjectType",
        "ObjectPlacement",
        "Representation",
        "LongName",
        "CompositionType",
        "Elevation",
        "RefLatitude",
        "RefLongitude",
        "RefElevation",
    ];
    DEFAULT_NAMES.get(index).copied().unwrap_or("Value")
}

fn scalar_value_and_type(value: &StepValue) -> Option<(serde_json::Value, &'static str)> {
    match value {
        StepValue::String(value) => Some((json!(value.as_str()), "IFCLABEL")),
        StepValue::Enum(value) => Some((json!(value.as_str()), "IFCLABEL")),
        StepValue::Typed { type_name, value } => {
            let inner = scalar_json_value(value)?;
            Some((inner, leak_type_name(type_name)))
        }
        StepValue::Real(value) => Some((json!(value), "IFCREAL")),
        StepValue::Int(value) => Some((json!(value), "IFCINTEGER")),
        StepValue::Bool(value) => Some((json!(value), "IFCBOOLEAN")),
        _ => None,
    }
}

fn scalar_json_value(value: &StepValue) -> Option<serde_json::Value> {
    match value {
        StepValue::String(value) => Some(json!(value.as_str())),
        StepValue::Enum(value) => Some(json!(value.as_str())),
        StepValue::Real(value) => Some(json!(value)),
        StepValue::Int(value) => Some(json!(value)),
        StepValue::Bool(value) => Some(json!(value)),
        StepValue::Typed { value, .. } => scalar_json_value(value),
        _ => None,
    }
}

fn leak_type_name(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn encode_scalar_list_attribute(name: &str, items: &[StepValue]) -> Option<String> {
    let mut values = Vec::new();
    let mut kind = "UNDEFINED";
    for item in items {
        let (value, item_kind) = scalar_value_and_type(item)?;
        kind = item_kind;
        values.push(value);
    }
    Some(json!([name, values, kind]).to_string())
}

fn absolute_elevation(step: &StepFile, id: u64) -> f64 {
    let Some(entity) = step.entities.get(&id) else {
        return 0.0;
    };
    let Some(mut placement) = entity.args.get(5).and_then(StepValue::as_ref) else {
        return 0.0;
    };
    let mut elevation = 0.0;
    loop {
        let Some(local_placement) = step.entities.get(&placement) else {
            break;
        };
        if let Some(relative_placement) = local_placement.args.get(1).and_then(StepValue::as_ref) {
            if let Some(axis) = step.entities.get(&relative_placement) {
                if let Some(location) = axis.args.first().and_then(StepValue::as_ref) {
                    if let Some(point) = step.entities.get(&location) {
                        if let Some(coords) = point.args.first().and_then(StepValue::as_list) {
                            elevation += coords.get(2).and_then(StepValue::as_real).unwrap_or(0.0);
                        }
                    }
                }
            }
        }
        if let Some(parent) = local_placement.args.first().and_then(StepValue::as_ref) {
            placement = parent;
        } else {
            break;
        }
    }
    elevation
}

fn json_number(value: f64) -> serde_json::Value {
    if (value.fract()).abs() < f64::EPSILON {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn to_u32(value: u64) -> Result<u32, FragmentsError> {
    u32::try_from(value).map_err(|_| FragmentsError::IntegerOverflow)
}

fn compress(bytes: &[u8]) -> Result<Vec<u8>, FragmentsError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|e| FragmentsError::Compression(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| FragmentsError::Compression(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use ifc_model::build_model;
    use ifc_step::parse_step_file;

    use super::*;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn converts_digitalhub_to_fragments() {
        let path = workspace_root().join("web/wasm-prototype/public/DigitalHub.ifc");
        if !path.exists() {
            return;
        }
        let step = parse_step_file(&path).expect("parse DigitalHub.ifc");
        let model = build_model(&step).expect("build model");
        let bytes = convert_step_to_fragments(&model, &step, &FragmentsConfig::default())
            .expect("convert fragments");
        assert!(!bytes.raw.is_empty());
        assert!(!bytes.compressed.is_empty());
        assert!(model_buffer_has_identifier(&bytes.raw));
    }
}
