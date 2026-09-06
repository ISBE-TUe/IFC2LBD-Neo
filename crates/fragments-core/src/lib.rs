mod convert;
mod shell_processor;
pub mod step;

pub use convert::{
    convert_step_to_fragments, FragmentsBytes, FragmentsConfig, FragmentsError,
    build_entity_section, collect_serialized_entity_ids, EntitySection,
};
pub use shell_processor::{get_raw_shell_data, get_shell_data, ShellOutput};
pub use step::{geometry_instances_for_product, product_world_transform, Affine3, GeometryInstance, ShellGeometry};
pub use convert::hash_shell;
/// Stable hashing primitives, shared with `plugin-geometry-producer` — which owns the
/// per-element geometry hash that gets written into RDF and compared across revisions.
/// See the note above them in `convert.rs` for why they exist rather than reusing
/// `hash_shell` or `hash_ifc_mesh`.
pub use convert::{fnv_feed, quantise, FNV_OFFSET, FNV_PRIME, HASH_PRECISION};
