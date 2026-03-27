use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use ifc_model::build_model;
use ifc_model::IfcModel;
use ifc_step::{parse_step_file, EntityId, StepFile, StepValue};
use lbd_converter::{stream_step_and_model, ConvertOptions};
use lbd_geometry::{
    derive_relations_with_exact_kernel_subprocess_batch, BoundingBox, ExactCheckOptions,
    GeometryRelation, GeometryRelationKind, SubprocessKernelExecutionOptions,
};
use lbd_serializer::{
    serialize_lbd_batches_to_writer, serialize_nquads_merged_batches_to_writer,
    serialize_turtle_batches_to_writer,
};
use rayon::prelude::*;
use serde::Serialize;

mod mesh;
mod transform;
mod voxel;

const SERIALIZER_CHANNEL_CAPACITY: usize = 32;
const SERIALIZER_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Turtle,
    Nquads,
}

#[derive(Debug, Parser)]
#[command(name = "ifc2lbd-neo")]
#[command(about = "Convert IFC STEP files to a first-slice LBD Turtle model")]
struct Args {
    input: PathBuf,

    #[arg(
        short = 'o',
        short_alias = 't',
        long = "output",
        visible_alias = "target-file"
    )]
    output_file: Option<PathBuf>,

    #[arg(
        short = 'u',
        long = "base-uri",
        visible_alias = "url",
        default_value = "https://lbd.example.com/"
    )]
    base_uri: String,

    /// Output syntax. `nquads` writes LBD+IfcOWL into one `.nq` stream with named graphs.
    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Turtle)]
    output_format: OutputFormat,

    /// Override named graph IRI for LBD triples in `nquads` mode.
    #[arg(long = "lbd-graph-iri")]
    lbd_graph_iri: Option<String>,

    /// Override named graph IRI for IfcOWL triples in `nquads` mode.
    #[arg(long = "ifcowl-graph-iri")]
    ifcowl_graph_iri: Option<String>,

    /// Emit full IfcOWL-compatible output and links in a separate sidecar file.
    #[arg(long = "ifcowl", default_value_t = false)]
    ifcowl: bool,

    /// Enable BOT topology from IFC relationship evidence only (no geometry adjacency).
    #[arg(long = "topology", default_value_t = false)]
    topology: bool,

    /// Enable full topology mode with OCC exact geometry checks.
    #[arg(long = "topology-full", default_value_t = false)]
    topology_full: bool,

    /// Emit per-element bounding boxes in LBD output.
    #[arg(long = "bbox", default_value_t = false)]
    bbox: bool,

    /// Escalate bbox extraction to exact world-vertex AABB when transformed local-box
    /// volume inflation exceeds this threshold.
    #[arg(long = "bbox-inflation-threshold", default_value_t = 1.5, hide = true)]
    bbox_inflation_threshold: f64,

    /// Optional JSON report path with bbox quality stats and top outliers.
    #[arg(long = "bbox-report")]
    bbox_report: Option<PathBuf>,

    /// Development tuning.
    #[arg(long = "geometry-tolerance", default_value_t = 1e-6, hide = true)]
    geometry_tolerance: f64,

    /// Voxel cell size in meters (default 0.1 = 10cm).
    #[arg(long = "voxel-cell-size", default_value_t = 0.1, hide = true)]
    voxel_cell_size: f64,

    /// Skip elements whose voxel count exceeds this threshold (default 50000).
    /// Giant-footprint elements (e.g. BuildingElementProxy spanning a whole storey)
    /// produce enormous voxel sets and cause false-positive adjacency with nearly
    /// every other element. Setting to 0 disables the filter.
    #[arg(
        long = "voxel-max-element-voxels",
        default_value_t = 50_000,
        hide = true
    )]
    voxel_max_element_voxels: usize,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();
    let output_format = args.output_format;

    if args.topology && args.topology_full {
        anyhow::bail!("use either --topology or --topology-full, not both");
    }

    let emit_ifcowl = args.ifcowl || output_format == OutputFormat::Nquads;
    if output_format == OutputFormat::Nquads && !args.ifcowl {
        tracing::info!("nquads mode enabled: forcing IfcOWL emission into named graph output");
    }
    let normalized_base = normalize_base_for_graph_iri(&args.base_uri);
    let lbd_graph_iri = args
        .lbd_graph_iri
        .clone()
        .unwrap_or_else(|| format!("{normalized_base}/lbd"));
    let ifcowl_graph_iri = args
        .ifcowl_graph_iri
        .clone()
        .unwrap_or_else(|| format!("{normalized_base}/ifcowl"));

    let step = parse_step_file(&args.input)
        .with_context(|| format!("failed to parse STEP file {}", args.input.display()))?;
    let model = build_model(&step).context("failed to build IFC model")?;

    let topology_enabled = args.topology || args.topology_full;
    let derive_adjacency = args.topology_full;
    let mut geometry_bounding_boxes: Option<Arc<HashMap<EntityId, BoundingBox>>> = None;
    let mut geometry_wkts: Option<Arc<HashMap<EntityId, String>>> = None;
    let geometry_relations = if derive_adjacency {
        let full_start = Instant::now();
        let (relations, mesh_bboxes, mesh_wkts, bbox_report) = topology_full_occ_relations(
            &model,
            &step,
            &args.input,
            args.geometry_tolerance,
            args.bbox_inflation_threshold,
        )?;
        tracing::info!(
            "topology-full OCC produced {} relations in {:.3}s",
            relations.len(),
            full_start.elapsed().as_secs_f64(),
        );
        if let Some(path) = args.bbox_report.as_ref() {
            let report_json = serde_json::to_string_pretty(&bbox_report)
                .context("failed to serialize bbox report JSON")?;
            std::fs::write(path, report_json)
                .with_context(|| format!("failed to write bbox report {}", path.display()))?;
        }
        if args.bbox {
            geometry_bounding_boxes = Some(arc_bounding_boxes_from_raw(mesh_bboxes));
            geometry_wkts = Some(Arc::new(mesh_wkts));
        }
        Some(Arc::new(relations))
    } else {
        None
    };

    if args.bbox && geometry_bounding_boxes.is_none() {
        let bbox_start = Instant::now();
        let (mesh_bboxes, mesh_wkts, bbox_report) = collect_mesh_bounding_boxes_hybrid(
            &step,
            model.elements.keys().copied().collect(),
            args.bbox_inflation_threshold,
        );
        tracing::info!(
            "bbox extraction produced {} bboxes in {:.3}s (exact escalations: {} / {}, avg inflation fast/final: {:.3}/{:.3}, max fast/final: {:.3}/{:.3})",
            mesh_bboxes.len(),
            bbox_start.elapsed().as_secs_f64(),
            bbox_report.escalated_exact_count,
            bbox_report.elements_with_mesh,
            bbox_report.avg_inflation_fast,
            bbox_report.avg_inflation_final,
            bbox_report.max_inflation_fast,
            bbox_report.max_inflation_final,
        );
        if let Some(path) = args.bbox_report.as_ref() {
            let report_json = serde_json::to_string_pretty(&bbox_report)
                .context("failed to serialize bbox report JSON")?;
            std::fs::write(path, report_json)
                .with_context(|| format!("failed to write bbox report {}", path.display()))?;
        }
        geometry_bounding_boxes = Some(arc_bounding_boxes_from_raw(mesh_bboxes));
        geometry_wkts = Some(Arc::new(mesh_wkts));
    }

    let options = ConvertOptions {
        base_uri: args.base_uri,
        emit_ifcowl_links: emit_ifcowl,
        enable_topology: topology_enabled,
        enable_topology_extension: args.topology_full,
        geometry_relations,
        geometry_bounding_boxes,
        geometry_wkts,
        geometry_tolerance: args.geometry_tolerance,
    };

    let (converter_lbd_sender, converter_lbd_receiver) =
        crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
    let lbd_receiver = converter_lbd_receiver;

    let (ifcowl_sender, mut ifcowl_receiver) = if emit_ifcowl {
        let (sender, receiver) = crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };

    let lbd_target = args.output_file.clone();
    let lbd_base_uri = options.base_uri.clone();
    let lbd_graph_iri_thread = lbd_graph_iri.clone();
    let ifcowl_graph_iri_thread = ifcowl_graph_iri.clone();
    let merged_ifcowl_receiver = if output_format == OutputFormat::Nquads {
        Some(
            ifcowl_receiver
                .take()
                .ok_or_else(|| anyhow::anyhow!("nquads mode requires IfcOWL receiver channel"))?,
        )
    } else {
        None
    };
    let lbd_thread = thread::spawn(move || -> anyhow::Result<()> {
        match output_format {
            OutputFormat::Turtle => match lbd_target {
                Some(path) => {
                    let file = File::create(&path).with_context(|| {
                        format!("failed to create output file {}", path.display())
                    })?;
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
                    serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                        .with_context(|| format!("failed to write Turtle to {}", path.display()))?;
                }
                None => {
                    let stdout = std::io::stdout();
                    let handle = stdout.lock();
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                    serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                        .context("failed to write Turtle to stdout")?;
                }
            },
            OutputFormat::Nquads => {
                let ifcowl_receiver = merged_ifcowl_receiver
                    .ok_or_else(|| anyhow::anyhow!("missing IfcOWL receiver for nquads mode"))?;
                match lbd_target {
                    Some(path) => {
                        let file = File::create(&path).with_context(|| {
                            format!("failed to create output file {}", path.display())
                        })?;
                        let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
                        serialize_nquads_merged_batches_to_writer(
                            lbd_receiver,
                            ifcowl_receiver,
                            writer,
                            &lbd_graph_iri_thread,
                            &ifcowl_graph_iri_thread,
                        )
                        .with_context(|| {
                            format!("failed to write N-Quads to {}", path.display())
                        })?;
                    }
                    None => {
                        let stdout = std::io::stdout();
                        let handle = stdout.lock();
                        let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                        serialize_nquads_merged_batches_to_writer(
                            lbd_receiver,
                            ifcowl_receiver,
                            writer,
                            &lbd_graph_iri_thread,
                            &ifcowl_graph_iri_thread,
                        )
                        .context("failed to write N-Quads to stdout")?;
                    }
                }
            }
        }
        Ok(())
    });

    let mut ifcowl_thread = None;
    if output_format == OutputFormat::Turtle && emit_ifcowl {
        let receiver = ifcowl_receiver
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing IfcOWL receiver for turtle sidecar mode"))?;
        let path = resolve_ifcowl_path(args.output_file.as_deref(), &args.input);
        let ifcowl_base = options.base_uri.clone();
        ifcowl_thread = Some(thread::spawn(move || -> anyhow::Result<()> {
            let file = File::create(&path).with_context(|| {
                format!("failed to create IfcOWL output file {}", path.display())
            })?;
            let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
            serialize_turtle_batches_to_writer(receiver, writer, Some(&ifcowl_base))
                .with_context(|| format!("failed to write IfcOWL Turtle to {}", path.display()))?;
            Ok(())
        }));
    }

    stream_step_and_model(
        &step,
        &model,
        &options,
        &converter_lbd_sender,
        ifcowl_sender.as_ref(),
    )
    .context("failed to stream conversion output")?;
    drop(converter_lbd_sender);
    drop(ifcowl_sender);

    lbd_thread
        .join()
        .map_err(|_| anyhow::anyhow!("LBD serializer thread panicked"))??;

    if let Some(thread) = ifcowl_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("IfcOWL serializer thread panicked"))??;
    }

    Ok(())
}

fn topology_full_occ_relations(
    model: &IfcModel,
    step: &StepFile,
    input_path: &Path,
    geometry_tolerance: f64,
    bbox_inflation_threshold: f64,
) -> anyhow::Result<(
    Vec<GeometryRelation>,
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    BboxQualityReport,
)> {
    let (candidate_pairs, mut prefilter_bboxes) = semantic_candidate_pairs(model, step);
    let unique_elements = {
        let mut ids = HashSet::new();
        for (a, b) in &candidate_pairs {
            ids.insert(*a);
            ids.insert(*b);
        }
        ids
    };
    tracing::info!(
        "topology-full candidates: {} pairs across {} elements",
        candidate_pairs.len(),
        unique_elements.len(),
    );

    if candidate_pairs.is_empty() {
        let empty_report = BboxQualityReport {
            elements_requested: 0,
            elements_with_mesh: 0,
            escalated_exact_count: 0,
            rotated_bbox_count: 0,
            avg_inflation_fast: 0.0,
            max_inflation_fast: 0.0,
            avg_inflation_final: 0.0,
            max_inflation_final: 0.0,
            avg_escalated_reduction_ratio: 0.0,
            count_fast_over_1_2: 0,
            count_fast_over_1_5: 0,
            count_fast_over_1_8: 0,
            count_fast_over_2_0: 0,
            inflation_threshold: bbox_inflation_threshold,
            top_inflation_outliers: Vec::new(),
        };
        return Ok((Vec::new(), HashMap::new(), HashMap::new(), empty_report));
    }

    let mut sorted_element_ids: Vec<EntityId> = unique_elements.iter().copied().collect();
    sorted_element_ids.sort_unstable();
    let (mesh_bboxes, mesh_wkts, bbox_report) =
        collect_mesh_bounding_boxes_hybrid(step, sorted_element_ids, bbox_inflation_threshold);

    for (eid, bbox) in mesh_bboxes.iter() {
        prefilter_bboxes.entry(*eid).or_insert(*bbox);
    }

    let kernel_bin = resolve_geometry_kernel_bin()?;
    let (kernel_args, _cache_guard) = prepare_kernel_cache_args(input_path)?;
    tracing::info!("topology-full OCC kernel: {}", kernel_bin.display());

    let options = ExactCheckOptions {
        tolerance: geometry_tolerance,
    };
    let execution = SubprocessKernelExecutionOptions {
        timeout: Duration::from_secs(600),
        // Keep one kernel invocation for typical model sizes to avoid rebuilding
        // in-memory shape maps across multiple subprocess calls.
        max_pairs_per_batch: 50_000,
    };

    let relations = derive_relations_with_exact_kernel_subprocess_batch(
        model,
        kernel_bin,
        kernel_args,
        input_path.to_path_buf(),
        &candidate_pairs,
        &options,
        &execution,
        &prefilter_bboxes,
    )
    .context("exact OCC topology kernel failed")?;

    let intersecting_triples = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::IntersectingElement)
        .count();
    let interface_of_triples = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::InterfaceOf)
        .count();
    let interface_nodes = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::InterfaceOf)
        .map(|r| r.source)
        .collect::<HashSet<_>>()
        .len();
    tracing::info!(
        "topology-full OCC relations: intersecting triples={}, interfaceOf triples={}, intersecting pairs={}, interface nodes={}",
        intersecting_triples,
        interface_of_triples,
        intersecting_triples / 2,
        interface_nodes,
    );

    Ok((relations, mesh_bboxes, mesh_wkts, bbox_report))
}

#[derive(Debug)]
struct CacheDirGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl Drop for CacheDirGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                tracing::warn!(
                    "failed to remove temporary OCC cache dir {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
    }
}

fn prepare_kernel_cache_args(input_path: &Path) -> anyhow::Result<(Vec<String>, CacheDirGuard)> {
    if let Ok(override_dir) = std::env::var("IFC2LBD_OCC_CACHE_DIR") {
        let path = PathBuf::from(override_dir);
        std::fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create IFC2LBD_OCC_CACHE_DIR at {}",
                path.display()
            )
        })?;
        return Ok((
            vec![
                "--brep-cache-dir".to_string(),
                path.to_string_lossy().into_owned(),
            ],
            CacheDirGuard {
                path,
                cleanup_on_drop: false,
            },
        ));
    }

    let keep_temp_cache = std::env::var("IFC2LBD_OCC_CACHE_PERSIST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc");
    let safe_stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let path = std::env::temp_dir()
        .join("ifc2lbd-neo-occ-cache")
        .join(format!("{safe_stem}_{pid}_{now}"));
    std::fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create temporary OCC cache dir {}",
            path.display()
        )
    })?;
    tracing::info!(
        "topology-full OCC cache dir: {}{}",
        path.display(),
        if keep_temp_cache {
            " (persist=true)"
        } else {
            " (ephemeral)"
        }
    );
    Ok((
        vec![
            "--brep-cache-dir".to_string(),
            path.to_string_lossy().into_owned(),
        ],
        CacheDirGuard {
            path,
            cleanup_on_drop: !keep_temp_cache,
        },
    ))
}

fn resolve_geometry_kernel_bin() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("IFC2LBD_GEOMETRY_KERNEL_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Ok(p);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("lbd-geometry-kernel"));
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    candidates.push(workspace_root.join("target/release/lbd-geometry-kernel"));
    candidates.push(workspace_root.join("target/debug/lbd-geometry-kernel"));

    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Ok(found);
    }

    tracing::info!("building lbd-geometry-kernel once (auto-discovery path)");
    let mut cargo_build = Command::new("cargo");
    cargo_build
        .arg("build")
        .arg("-p")
        .arg("lbd-geometry-kernel")
        .arg("--bin")
        .arg("lbd-geometry-kernel")
        .current_dir(&workspace_root);
    configure_pyo3_python_env(&mut cargo_build);
    let status = cargo_build
        .status()
        .context("failed to start cargo build for lbd-geometry-kernel")?;
    if !status.success() {
        anyhow::bail!(
            "failed to build lbd-geometry-kernel automatically (status: {})",
            status
        );
    }

    let built = workspace_root.join("target/debug/lbd-geometry-kernel");
    if built.is_file() {
        Ok(built)
    } else {
        anyhow::bail!(
            "lbd-geometry-kernel build finished but binary was not found at {}",
            built.display()
        )
    }
}

fn configure_pyo3_python_env(cmd: &mut Command) {
    if std::env::var_os("PYO3_PYTHON").is_some() {
        return;
    }
    if let Some(python) = detect_python3_executable() {
        tracing::info!("using detected python for pyo3: {}", python.display());
        cmd.env("PYO3_PYTHON", python);
    }
}

fn detect_python3_executable() -> Option<PathBuf> {
    let output = Command::new("python3")
        .arg("-c")
        .arg("import sys; print(sys.executable)")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let resolved = PathBuf::from(path);
    if resolved.is_file() {
        Some(resolved)
    } else {
        None
    }
}

/// Extract an approximate axis-aligned bounding box for an IFC product from raw STEP entities.
/// Walks the representation tree and collects 3D coordinate values without building OCC geometry.
/// Returns [minX, minY, minZ, maxX, maxY, maxZ] or None if no coordinates are found.
fn approximate_bbox(step: &StepFile, element_id: EntityId) -> Option<[f64; 6]> {
    let entity = step.entities.get(&element_id)?;
    // ObjectPlacement is args[5] for most IfcProduct subtypes.
    let placement_id = match entity.args.get(5) {
        Some(StepValue::Ref(id)) => *id,
        _ => return None,
    };
    // Walk the placement chain to get the world translation (approximate: take the
    // last LocalPlacement translation, ignoring rotation and parent transforms).
    // Good enough for spatial pre-filtering within a single storey.
    let world_translate = placement_translation(step, placement_id);

    // Collect all 3D coordinate values from the representation items.
    let rep_id = match entity.args.get(6) {
        Some(StepValue::Ref(id)) => *id,
        _ => {
            // No representation — use placement origin as a point bbox.
            let [x, y, z] = world_translate;
            return Some([x, y, z, x, y, z]);
        }
    };
    let mut pts: Vec<[f64; 3]> = Vec::new();
    collect_points(step, rep_id, &mut pts, 0, 300);
    // Elements with > 300 coordinate points have complex/freeform geometry (furniture,
    // appliances, MEP). These are never structural and we skip them for topology analysis.
    if pts.len() >= 300 {
        return None;
    }
    if pts.is_empty() {
        let [x, y, z] = world_translate;
        return Some([x, y, z, x, y, z]);
    }
    // Apply the world translation to all collected points.
    let [tx, ty, tz] = world_translate;
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    for [x, y, z] in &pts {
        let wx = x + tx;
        let wy = y + ty;
        let wz = z + tz;
        min[0] = min[0].min(wx);
        min[1] = min[1].min(wy);
        min[2] = min[2].min(wz);
        max[0] = max[0].max(wx);
        max[1] = max[1].max(wy);
        max[2] = max[2].max(wz);
    }
    Some([min[0], min[1], min[2], max[0], max[1], max[2]])
}

/// Walk a placement chain and return the accumulated translation (sum of all local origins).
/// Ignores rotation for speed — sufficient for spatial pre-filtering.
fn placement_translation(step: &StepFile, placement_id: EntityId) -> [f64; 3] {
    let mut tx = 0.0f64;
    let mut ty = 0.0f64;
    let mut tz = 0.0f64;
    let mut current_id = placement_id;
    let mut depth = 0;
    loop {
        if depth > 20 {
            break;
        } // guard against cycles
        depth += 1;
        let Some(entity) = step.entities.get(&current_id) else {
            break;
        };
        match entity.entity_name.as_str() {
            "IFCLOCALPLACEMENT" => {
                // args[0] = PlacementRelTo (parent, optional), args[1] = RelativePlacement
                let rel_id = match entity.args.get(1) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => break,
                };
                let [lx, ly, lz] = axis2placement3d_origin(step, rel_id);
                tx += lx;
                ty += ly;
                tz += lz;
                match entity.args.first() {
                    Some(StepValue::Ref(parent_id)) => {
                        current_id = *parent_id;
                    }
                    _ => break,
                }
            }
            _ => break,
        }
    }
    [tx, ty, tz]
}

fn axis2placement3d_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
        return [0.0, 0.0, 0.0];
    }
    let loc_id = match entity.args.first() {
        Some(StepValue::Ref(id)) => *id,
        _ => return [0.0, 0.0, 0.0],
    };
    cartesian_point_3d(step, loc_id)
}

fn cartesian_point_3d(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
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

/// Recursively collect 3D coordinate values from an IFC entity tree.
/// Stops at depth 10 to avoid runaway traversal.
fn collect_points(
    step: &StepFile,
    id: EntityId,
    pts: &mut Vec<[f64; 3]>,
    depth: usize,
    max: usize,
) {
    if depth > 10 || pts.len() >= max {
        return;
    }
    let Some(entity) = step.entities.get(&id) else {
        return;
    };
    match entity.entity_name.as_str() {
        "IFCCARTESIANPOINT" => {
            if let Some(StepValue::List(coords)) = entity.args.first() {
                if coords.len() >= 3 {
                    let x = coords[0].as_real().unwrap_or(0.0);
                    let y = coords[1].as_real().unwrap_or(0.0);
                    let z = coords[2].as_real().unwrap_or(0.0);
                    pts.push([x, y, z]);
                }
            }
        }
        "IFCCARTESIANPOINTLIST3D" => {
            if let Some(StepValue::List(list)) = entity.args.first() {
                for item in list {
                    if let StepValue::List(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_real().unwrap_or(0.0);
                            let y = coords[1].as_real().unwrap_or(0.0);
                            let z = coords[2].as_real().unwrap_or(0.0);
                            pts.push([x, y, z]);
                        }
                    }
                }
            }
        }
        _ => {
            // Walk references in args.
            for arg in &entity.args {
                match arg {
                    StepValue::Ref(child_id) => {
                        collect_points(step, *child_id, pts, depth + 1, max);
                    }
                    StepValue::List(list) => {
                        for item in list {
                            if pts.len() >= max {
                                return;
                            }
                            if let StepValue::Ref(child_id) = item {
                                collect_points(step, *child_id, pts, depth + 1, max);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn bboxes_overlap_3d(a: &[f64; 6], b: &[f64; 6], tolerance: f64) -> bool {
    // X
    a[0] - tolerance <= b[3] + tolerance
        && a[3] + tolerance >= b[0] - tolerance
    // Y
        && a[1] - tolerance <= b[4] + tolerance
        && a[4] + tolerance >= b[1] - tolerance
    // Z
        && a[2] - tolerance <= b[5] + tolerance
        && a[5] + tolerance >= b[2] - tolerance
}

/// IFC element types that can generate bot:Interface (shared surfaces between structural elements).
/// Furniture, MEP, sanitary, distribution, and annotation elements are excluded — they
/// never touch structural elements in a surface-sharing sense.
fn is_structural_ifc_type(entity_name: &str) -> bool {
    matches!(
        entity_name,
        "IFCWALL"
            | "IFCWALLSTANDARDCASE"
            | "IFCSLAB"
            | "IFCCOLUMN"
            | "IFCBEAM"
            | "IFCROOF"
            | "IFCCOVERING"
            | "IFCCURTAINWALL"
            | "IFCPLATE"
            | "IFCMEMBER"
            | "IFCDOOR"
            | "IFCWINDOW"
            | "IFCSTAIR"
            | "IFCSTAIRFLIGHT"
            | "IFCRAMP"
            | "IFCRAMPFLIGHT"
            | "IFCFOOTING"
            | "IFCPILE"
            | "IFCBUILDINGELEMENTPROXY"
    )
}

fn semantic_candidate_pairs(
    model: &IfcModel,
    step: &StepFile,
) -> (Vec<(EntityId, EntityId)>, HashMap<EntityId, [f64; 6]>) {
    use std::collections::HashSet;
    let mut by_space: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    for boundary in &model.rel_space_boundaries {
        let Some(element) = boundary.element else {
            continue;
        };
        if model.elements.contains_key(&element) {
            by_space.entry(boundary.space).or_default().push(element);
        }
    }

    let mut pairs = HashSet::new();
    for elements in by_space.values_mut() {
        elements.sort_unstable();
        elements.dedup();
        for i in 0..elements.len() {
            for j in (i + 1)..elements.len() {
                let a = elements[i];
                let b = elements[j];
                let canonical = if a < b { (a, b) } else { (b, a) };
                pairs.insert(canonical);
            }
        }
    }

    // If we found pairs from space boundaries, return them (no bboxes needed — space boundary path).
    if !pairs.is_empty() {
        let mut out: Vec<_> = pairs.into_iter().collect();
        out.sort_unstable();
        return (out, HashMap::new());
    }

    // Fallback: no IfcRelSpaceBoundary records — group elements by storey/structure containment.
    // Only consider structural/architectural elements that can generate bot:Interface.
    // Furniture, MEP, and distribution elements never share structural surfaces.
    tracing::info!(
        "No IfcRelSpaceBoundary records found; falling back to storey-scoped candidate pairs (structural elements only)"
    );
    let mut by_structure: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    for (&element_id, &structure_id) in &model.contained_in {
        if let Some(node) = model.elements.get(&element_id) {
            if is_structural_ifc_type(node.entity_name.as_str()) {
                by_structure
                    .entry(structure_id)
                    .or_default()
                    .push(element_id);
            }
        }
    }
    // Compute approximate bboxes for all candidate elements from STEP data (pure Rust, fast).
    let mut element_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::new();
    for elements in by_structure.values() {
        for &id in elements {
            if let Some(bbox) = approximate_bbox(step, id) {
                element_bboxes.insert(id, bbox);
            }
        }
    }

    // Generate pairs only where bboxes overlap (XY plane, 5cm tolerance).
    // Adjacent elements share a face so their bboxes truly overlap; 5cm covers
    // placement approximation errors without pairing distant elements.
    const BBOX_TOLERANCE: f64 = 0.05; // 5cm — touching elements have overlapping bboxes
    for elements in by_structure.values_mut() {
        elements.sort_unstable();
        elements.dedup();
        for i in 0..elements.len() {
            for j in (i + 1)..elements.len() {
                let a = elements[i];
                let b = elements[j];
                // Require both elements to have a bbox. Elements without a bbox have
                // complex/freeform geometry (furniture, MEP) or no geometry — skip them.
                // Among elements with bboxes, only pair those whose bboxes overlap.
                match (element_bboxes.get(&a), element_bboxes.get(&b)) {
                    (Some(ba), Some(bb)) => {
                        if !bboxes_overlap_3d(ba, bb, BBOX_TOLERANCE) {
                            continue;
                        }
                    }
                    _ => continue,
                }
                let canonical = if a < b { (a, b) } else { (b, a) };
                pairs.insert(canonical);
            }
        }
    }
    if pairs.len() > 100_000 {
        tracing::warn!(
            "storey-scoped candidate pairs ({}) exceeds 100k — this is unexpected after bbox filtering",
            pairs.len()
        );
    }

    let mut out: Vec<_> = pairs.into_iter().collect();
    out.sort_unstable();
    (out, element_bboxes)
}

/// Voxel-based adjacency detection: extract meshes, voxelize, and check adjacency.
///
/// 1. Generate candidate pairs (same as before: storey-scoped, bbox-filtered)
/// 2. For each unique element in the pairs, extract its triangle mesh and voxelize it
/// 3. For each candidate pair, check voxel adjacency
/// 4. Return GeometryRelation::AdjacentElement for each adjacent pair
/// Returns (relations, mesh_bboxes) where mesh_bboxes maps EntityId → [xmin,ymin,zmin,xmax,ymax,zmax]
/// computed from the actual triangle mesh in world coordinates.
fn voxel_adjacency_relations(
    model: &IfcModel,
    step: &StepFile,
    cell_size: f64,
    max_element_voxels: usize,
) -> (Vec<GeometryRelation>, HashMap<EntityId, [f64; 6]>) {
    let (candidates, _element_bboxes) = semantic_candidate_pairs(model, step);
    tracing::info!(
        "voxel adjacency: {} candidate pairs from {} structural elements",
        candidates.len(),
        {
            let mut ids = HashSet::new();
            for (a, b) in &candidates {
                ids.insert(*a);
                ids.insert(*b);
            }
            ids.len()
        }
    );

    if candidates.is_empty() {
        return (Vec::new(), HashMap::new());
    }

    // Collect unique element IDs
    let mut element_ids: Vec<EntityId> = {
        let mut ids = HashSet::new();
        for (a, b) in &candidates {
            ids.insert(*a);
            ids.insert(*b);
        }
        ids.into_iter().collect()
    };
    element_ids.sort_unstable();

    // Step 1: Extract and voxelize all elements in parallel; capture mesh bboxes.
    let voxel_start = Instant::now();
    let voxel_maps: Vec<(EntityId, HashSet<voxel::VoxelCoord>, [f64; 6])> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let mesh = mesh::extract_element_mesh(step, eid, &world_t);
            if mesh.is_empty() {
                tracing::debug!("element #{} has no mesh", eid);
                return None;
            }
            // Compute mesh bbox in world coordinates
            let mut mn = [f64::MAX; 3];
            let mut mx = [f64::MIN; 3];
            for chunk in mesh.vertices.chunks_exact(3) {
                for i in 0..3 {
                    mn[i] = mn[i].min(chunk[i]);
                    mx[i] = mx[i].max(chunk[i]);
                }
            }
            let bbox = [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]];
            let voxels = voxel::voxelize_triangles(&mesh.vertices, &mesh.indices, cell_size);
            if voxels.is_empty() {
                return None;
            }
            if max_element_voxels > 0 && voxels.len() > max_element_voxels {
                tracing::warn!(
                    "element #{} skipped: {} voxels exceeds limit {} (bbox {:.1}×{:.1}×{:.1}m)",
                    eid,
                    voxels.len(),
                    max_element_voxels,
                    mx[0] - mn[0],
                    mx[1] - mn[1],
                    mx[2] - mn[2],
                );
                return None;
            }
            Some((eid, voxels, bbox))
        })
        .collect();

    let mut mesh_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::with_capacity(voxel_maps.len());
    let voxel_map: HashMap<EntityId, HashSet<voxel::VoxelCoord>> = voxel_maps
        .into_iter()
        .map(|(eid, voxels, bbox)| {
            mesh_bboxes.insert(eid, bbox);
            (eid, voxels)
        })
        .collect();

    let meshed = voxel_map.len();
    let total_voxels: usize = voxel_map.values().map(|v| v.len()).sum();
    tracing::info!(
        "voxelized {}/{} elements ({} total voxels) in {:.3}s",
        meshed,
        element_ids.len(),
        total_voxels,
        voxel_start.elapsed().as_secs_f64(),
    );

    // Step 2: Check adjacency for all candidate pairs in parallel
    let adj_start = Instant::now();
    let adjacent_pairs: Vec<(EntityId, EntityId)> = candidates
        .par_iter()
        .filter_map(|&(a, b)| {
            let va = voxel_map.get(&a)?;
            let vb = voxel_map.get(&b)?;
            if voxel::voxels_adjacent(va, vb) {
                Some((a, b))
            } else {
                None
            }
        })
        .collect();

    // Step 3: Build proper BOT relations per spec:
    //   - bot:intersectingElement in both directions (element-element)
    //   - bot:Interface instance with bot:interfaceOf to both elements
    // bot:adjacentElement is Zone→Element only per BOT spec.
    // Synthetic interface IDs: use a range above any real entity ID.
    let max_entity_id = step.entities.keys().copied().max().unwrap_or(0);
    let mut relations = Vec::with_capacity(adjacent_pairs.len() * 4);
    for (i, &(a, b)) in adjacent_pairs.iter().enumerate() {
        let interface_id = max_entity_id + 1 + i as u64;
        // IntersectingElement both directions
        relations.push(GeometryRelation {
            source: a,
            target: b,
            kind: GeometryRelationKind::IntersectingElement,
        });
        relations.push(GeometryRelation {
            source: b,
            target: a,
            kind: GeometryRelationKind::IntersectingElement,
        });
        // InterfaceOf: synthetic interface → both elements
        relations.push(GeometryRelation {
            source: interface_id,
            target: a,
            kind: GeometryRelationKind::InterfaceOf,
        });
        relations.push(GeometryRelation {
            source: interface_id,
            target: b,
            kind: GeometryRelationKind::InterfaceOf,
        });
    }

    tracing::info!(
        "adjacency check: {} adjacent pairs found from {} candidates in {:.3}s",
        adjacent_pairs.len(),
        candidates.len(),
        adj_start.elapsed().as_secs_f64(),
    );

    (relations, mesh_bboxes)
}

#[derive(Debug, Clone, Serialize)]
struct BboxOutlier {
    entity_id: EntityId,
    inflation_fast: f64,
    inflation_final: f64,
    used_exact: bool,
    used_rotated: bool,
}

#[derive(Debug, Clone, Serialize)]
struct BboxQualityReport {
    elements_requested: usize,
    elements_with_mesh: usize,
    escalated_exact_count: usize,
    rotated_bbox_count: usize,
    avg_inflation_fast: f64,
    max_inflation_fast: f64,
    avg_inflation_final: f64,
    max_inflation_final: f64,
    avg_escalated_reduction_ratio: f64,
    count_fast_over_1_2: usize,
    count_fast_over_1_5: usize,
    count_fast_over_1_8: usize,
    count_fast_over_2_0: usize,
    inflation_threshold: f64,
    top_inflation_outliers: Vec<BboxOutlier>,
}

fn collect_mesh_bounding_boxes_hybrid(
    step: &StepFile,
    element_ids: Vec<EntityId>,
    inflation_threshold: f64,
) -> (
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    BboxQualityReport,
) {
    let records: Vec<(EntityId, [f64; 6], String, f64, f64, bool, bool)> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let local_mesh =
                mesh::extract_element_mesh(step, eid, &transform::Transform4::identity());
            if local_mesh.is_empty() {
                return None;
            }
            let local_bbox = bbox_from_vertices(&local_mesh.vertices)?;
            let local_volume = bbox_volume(&local_bbox);
            let fast_world_bbox = transform_aabb(&world_t, &local_bbox);
            let fast_world_volume = bbox_volume(&fast_world_bbox);
            let inflation = if local_volume > 1e-12 {
                fast_world_volume / local_volume
            } else {
                1.0
            };

            if inflation > inflation_threshold {
                let mut exact_mesh = local_mesh;
                exact_mesh.transform(&world_t);
                let exact_world_bbox = bbox_from_vertices(&exact_mesh.vertices)?;
                let exact_world_volume = bbox_volume(&exact_world_bbox);
                if let Some((wkt, obb_volume)) = oriented_bbox_wkt_xy(&exact_mesh.vertices) {
                    let final_inflation = if local_volume > 1e-12 {
                        obb_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        wkt,
                        inflation,
                        final_inflation,
                        true,
                        true,
                    ))
                } else {
                    let final_inflation = if local_volume > 1e-12 {
                        exact_world_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        bbox_wkt_polyhedral_surface_from_raw(&exact_world_bbox),
                        inflation,
                        final_inflation,
                        true,
                        false,
                    ))
                }
            } else {
                Some((
                    eid,
                    fast_world_bbox,
                    bbox_wkt_polyhedral_surface_from_raw(&fast_world_bbox),
                    inflation,
                    inflation,
                    false,
                    false,
                ))
            }
        })
        .collect();

    let mut out = HashMap::with_capacity(records.len());
    let mut wkts = HashMap::with_capacity(records.len());
    let mut sum_inflation_fast = 0.0_f64;
    let mut max_inflation_fast = 0.0_f64;
    let mut sum_inflation_final = 0.0_f64;
    let mut max_inflation_final = 0.0_f64;
    let mut escalated_exact_count = 0_usize;
    let mut escalated_reduction_sum = 0.0_f64;
    let mut count_fast_over_1_2 = 0_usize;
    let mut count_fast_over_1_5 = 0_usize;
    let mut count_fast_over_1_8 = 0_usize;
    let mut count_fast_over_2_0 = 0_usize;
    let mut outliers: Vec<BboxOutlier> = Vec::with_capacity(records.len());

    let mut rotated_bbox_count = 0_usize;
    for (eid, bbox, wkt, inflation_fast, inflation_final, escalated, used_rotated) in records {
        out.insert(eid, bbox);
        wkts.insert(eid, wkt);
        sum_inflation_fast += inflation_fast;
        max_inflation_fast = max_inflation_fast.max(inflation_fast);
        sum_inflation_final += inflation_final;
        max_inflation_final = max_inflation_final.max(inflation_final);
        if inflation_fast > 1.2 {
            count_fast_over_1_2 += 1;
        }
        if inflation_fast > 1.5 {
            count_fast_over_1_5 += 1;
        }
        if inflation_fast > 1.8 {
            count_fast_over_1_8 += 1;
        }
        if inflation_fast > 2.0 {
            count_fast_over_2_0 += 1;
        }
        if escalated {
            escalated_exact_count += 1;
            if inflation_fast > 1e-12 {
                escalated_reduction_sum += (inflation_fast - inflation_final) / inflation_fast;
            }
        }
        if used_rotated {
            rotated_bbox_count += 1;
        }
        outliers.push(BboxOutlier {
            entity_id: eid,
            inflation_fast,
            inflation_final,
            used_exact: escalated,
            used_rotated,
        });
    }

    outliers.sort_by(|a, b| b.inflation_fast.total_cmp(&a.inflation_fast));
    outliers.truncate(20);

    let elements_with_mesh = out.len();
    let avg_inflation_fast = if elements_with_mesh > 0 {
        sum_inflation_fast / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_inflation_final = if elements_with_mesh > 0 {
        sum_inflation_final / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_escalated_reduction_ratio = if escalated_exact_count > 0 {
        escalated_reduction_sum / escalated_exact_count as f64
    } else {
        0.0
    };
    (
        out,
        wkts,
        BboxQualityReport {
            elements_requested: element_ids.len(),
            elements_with_mesh,
            escalated_exact_count,
            rotated_bbox_count,
            avg_inflation_fast,
            max_inflation_fast,
            avg_inflation_final,
            max_inflation_final,
            avg_escalated_reduction_ratio,
            count_fast_over_1_2,
            count_fast_over_1_5,
            count_fast_over_1_8,
            count_fast_over_2_0,
            inflation_threshold,
            top_inflation_outliers: outliers,
        },
    )
}

fn bbox_from_vertices(vertices: &[f64]) -> Option<[f64; 6]> {
    if vertices.len() < 3 {
        return None;
    }
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    let mut any = false;
    for chunk in vertices.chunks_exact(3) {
        any = true;
        for i in 0..3 {
            mn[i] = mn[i].min(chunk[i]);
            mx[i] = mx[i].max(chunk[i]);
        }
    }
    if !any {
        return None;
    }
    Some([mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]])
}

fn bbox_volume(bbox: &[f64; 6]) -> f64 {
    let dx = (bbox[3] - bbox[0]).max(0.0);
    let dy = (bbox[4] - bbox[1]).max(0.0);
    let dz = (bbox[5] - bbox[2]).max(0.0);
    dx * dy * dz
}

fn transform_aabb(t: &transform::Transform4, bbox: &[f64; 6]) -> [f64; 6] {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let corners = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x0, y1, z0],
        [x1, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x0, y1, z1],
        [x1, y1, z1],
    ];
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    for p in corners {
        let tp = t.transform_point(&p);
        for i in 0..3 {
            mn[i] = mn[i].min(tp[i]);
            mx[i] = mx[i].max(tp[i]);
        }
    }
    [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]]
}

fn bbox_wkt_polyhedral_surface_from_raw(bbox: &[f64; 6]) -> String {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let x0 = fmt_num(x0);
    let y0 = fmt_num(y0);
    let z0 = fmt_num(z0);
    let x1 = fmt_num(x1);
    let y1 = fmt_num(y1);
    let z1 = fmt_num(z1);
    format!(
        "POLYHEDRALSURFACE Z ((({x0} {y0} {z0}, {x1} {y0} {z0}, {x1} {y1} {z0}, {x0} {y1} {z0}, {x0} {y0} {z0})), (({x0} {y0} {z1}, {x0} {y1} {z1}, {x1} {y1} {z1}, {x1} {y0} {z1}, {x0} {y0} {z1})), (({x0} {y0} {z0}, {x0} {y0} {z1}, {x1} {y0} {z1}, {x1} {y0} {z0}, {x0} {y0} {z0})), (({x1} {y0} {z0}, {x1} {y0} {z1}, {x1} {y1} {z1}, {x1} {y1} {z0}, {x1} {y0} {z0})), (({x1} {y1} {z0}, {x1} {y1} {z1}, {x0} {y1} {z1}, {x0} {y1} {z0}, {x1} {y1} {z0})), (({x0} {y1} {z0}, {x0} {y1} {z1}, {x0} {y0} {z1}, {x0} {y0} {z0}, {x0} {y1} {z0})))"
    )
}

fn oriented_bbox_wkt_xy(vertices: &[f64]) -> Option<(String, f64)> {
    if vertices.len() < 9 {
        return None;
    }
    let mut z_min = f64::MAX;
    let mut z_max = f64::MIN;
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(vertices.len() / 3);
    for p in vertices.chunks_exact(3) {
        pts.push((p[0], p[1]));
        z_min = z_min.min(p[2]);
        z_max = z_max.max(p[2]);
    }
    if pts.is_empty() {
        return None;
    }

    let n = pts.len() as f64;
    let (sum_x, sum_y) = pts
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let cx = sum_x / n;
    let cy = sum_y / n;

    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    // Principal direction in XY plane (PCA for 2D cloud)
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (ct, st) = (theta.cos(), theta.sin());
    let u = (ct, st);
    let v = (-st, ct);

    let mut u_min = f64::MAX;
    let mut u_max = f64::MIN;
    let mut v_min = f64::MAX;
    let mut v_max = f64::MIN;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        let pu = dx * u.0 + dy * u.1;
        let pv = dx * v.0 + dy * v.1;
        u_min = u_min.min(pu);
        u_max = u_max.max(pu);
        v_min = v_min.min(pv);
        v_max = v_max.max(pv);
    }
    let du = (u_max - u_min).max(0.0);
    let dv = (v_max - v_min).max(0.0);
    let dz = (z_max - z_min).max(0.0);
    if du <= f64::EPSILON || dv <= f64::EPSILON || dz <= f64::EPSILON {
        return None;
    }

    let corner_uv = [
        (u_min, v_min),
        (u_max, v_min),
        (u_max, v_max),
        (u_min, v_max),
    ];
    let mut cxy = [(0.0, 0.0); 4];
    for (i, (cu, cv)) in corner_uv.iter().enumerate() {
        cxy[i] = (cx + cu * u.0 + cv * v.0, cy + cu * u.1 + cv * v.1);
    }
    let (x0, y0) = cxy[0];
    let (x1, y1) = cxy[1];
    let (x2, y2) = cxy[2];
    let (x3, y3) = cxy[3];
    let z0 = fmt_num(z_min);
    let z1 = fmt_num(z_max);
    let wkt = format!(
        "POLYHEDRALSURFACE Z ((({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})))",
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x1), fmt_num(y1), z0, fmt_num(x2), fmt_num(y2), z0, fmt_num(x3), fmt_num(y3), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x0), fmt_num(y0), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x0), fmt_num(y0), z1,
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x0), fmt_num(y0), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x1), fmt_num(y1), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x1), fmt_num(y1), z0, fmt_num(x1), fmt_num(y1), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x2), fmt_num(y2), z0, fmt_num(x1), fmt_num(y1), z0,
        fmt_num(x2), fmt_num(y2), z0, fmt_num(x2), fmt_num(y2), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x3), fmt_num(y3), z0, fmt_num(x2), fmt_num(y2), z0,
        fmt_num(x3), fmt_num(y3), z0, fmt_num(x3), fmt_num(y3), z1, fmt_num(x0), fmt_num(y0), z1, fmt_num(x0), fmt_num(y0), z0, fmt_num(x3), fmt_num(y3), z0
    );
    Some((wkt, du * dv * dz))
}

fn fmt_num(v: f64) -> String {
    let mut s = format!("{v:.9}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Write element bounding boxes as GeoSPARQL WKT triples to a Turtle file.
///
/// Each element gets:
///   <element_iri> geo:hasGeometry <element_iri_geom> .
///   <element_iri_geom> a geo:Geometry ;
///       geo:asWKT "POLYGON Z ((...bottom face...))"^^geo:wktLiteral ;
///       geo:dimension 3 .
///
/// The WKT is a 3D polyhedron (6 faces of the bounding box) encoded as
/// POLYHEDRALSURFACE Z for maximum compatibility. The footprint POLYGON Z
/// (bottom face) plus a separate LINESTRING Z marking the height extent
/// is also included for 2D-capable tools.
fn arc_bounding_boxes_from_raw(
    raw: HashMap<EntityId, [f64; 6]>,
) -> Arc<HashMap<EntityId, BoundingBox>> {
    let mapped = raw
        .into_iter()
        .map(|(entity_id, [x_min, y_min, z_min, x_max, y_max, z_max])| {
            (
                entity_id,
                BoundingBox {
                    x_min,
                    x_max,
                    y_min,
                    y_max,
                    z_min,
                    z_max,
                },
            )
        })
        .collect();
    Arc::new(mapped)
}

fn resolve_ifcowl_path(output_file: Option<&Path>, input_file: &Path) -> PathBuf {
    if let Some(path) = output_file {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lbd_output");
        return parent.join(format!("{stem}_ifcowl.ttl"));
    }

    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc_output");
    PathBuf::from(format!("{stem}_ifcowl.ttl"))
}

fn normalize_base_for_graph_iri(base_uri: &str) -> String {
    base_uri.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::{Args, OutputFormat};
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn cli_defaults_are_minimal() {
        let args = Args::try_parse_from(["ifc2lbd-neo", "input.ifc"]).expect("parse");
        assert_eq!(args.output_format, OutputFormat::Turtle);
        assert!(args.lbd_graph_iri.is_none());
        assert!(args.ifcowl_graph_iri.is_none());
        assert!(!args.ifcowl);
        assert!(!args.topology);
        assert!(!args.topology_full);
        assert!(!args.bbox);
    }

    #[test]
    fn cli_parses_new_flags() {
        let args = Args::try_parse_from([
            "ifc2lbd-neo",
            "input.ifc",
            "--output",
            "out.ttl",
            "--base-uri",
            "https://example.test/base/",
            "--output-format",
            "nquads",
            "--lbd-graph-iri",
            "https://graphs.example.test/lbd",
            "--ifcowl-graph-iri",
            "https://graphs.example.test/ifcowl",
            "--ifcowl",
            "--topology-full",
            "--bbox",
        ])
        .expect("parse");
        assert_eq!(args.output_file.as_deref(), Some(Path::new("out.ttl")));
        assert_eq!(args.base_uri, "https://example.test/base/");
        assert_eq!(args.output_format, OutputFormat::Nquads);
        assert_eq!(
            args.lbd_graph_iri.as_deref(),
            Some("https://graphs.example.test/lbd")
        );
        assert_eq!(
            args.ifcowl_graph_iri.as_deref(),
            Some("https://graphs.example.test/ifcowl")
        );
        assert!(args.ifcowl);
        assert!(!args.topology);
        assert!(args.topology_full);
        assert!(args.bbox);
    }
}
