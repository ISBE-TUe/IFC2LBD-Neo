//! Structural diff tool for .frag files.
//!
//! Usage: frag-diff <file-a.frag> <file-b.frag>
//!
//! Decompresses both files, parses their FlatBuffer structures, and prints a
//! side-by-side comparison of all key field counts.

use std::io::Read;
use std::path::Path;

use flate2::read::ZlibDecoder;
use fragments_schema::*;

fn decompress(path: &Path) -> Vec<u8> {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut decoder = ZlibDecoder::new(data.as_slice());
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap_or_else(|e| panic!("decompress {}: {e}", path.display()));
    out
}

fn count_attrs(attrs: Option<flatbuffers::Vector<flatbuffers::ForwardsUOffset<Attribute>>>) -> usize {
    attrs.map(|v| v.len()).unwrap_or(0)
}

fn count_str_vec(v: Option<flatbuffers::Vector<flatbuffers::ForwardsUOffset<&str>>>) -> usize {
    v.map(|v| v.len()).unwrap_or(0)
}

fn count_u32_vec(v: Option<flatbuffers::Vector<u32>>) -> usize {
    v.map(|v| v.len()).unwrap_or(0)
}

fn print_model(label: &str, raw: &[u8]) {
    let model = root_as_model(raw).expect("invalid fragments FlatBuffer");

    let meshes = model.meshes();
    let shells = meshes.shells().len();
    let samples = meshes.samples().len();
    let reprs = meshes.representations().len();
    let materials = meshes.materials().len();
    let local_transforms = meshes.local_transforms().len();
    let global_transforms = meshes.global_transforms().len();
    let mesh_items = meshes.meshes_items().len();
    let mat_ids = count_u32_vec(meshes.material_ids());
    let rep_ids = count_u32_vec(meshes.representation_ids());
    let sample_ids = count_u32_vec(meshes.sample_ids());
    let lt_ids = count_u32_vec(meshes.local_transform_ids());
    let gt_ids = count_u32_vec(meshes.global_transform_ids());

    let local_ids = model.local_ids().len();
    let guids = model.guids().len();
    let categories = model.categories().len();
    let max_local_id = model.max_local_id();

    let attrs = count_attrs(model.attributes());
    let rels = model.relations().map(|v| v.len()).unwrap_or(0);
    let rel_items = model.relations_items().map(|v| v.len()).unwrap_or(0);
    let unique_attrs = count_str_vec(model.unique_attributes());
    let rel_names = count_str_vec(model.relation_names());

    println!("\n=== {label} ===");
    println!("  local_ids       : {local_ids}");
    println!("  guids           : {guids}");
    println!("  categories      : {categories}");
    println!("  max_local_id    : {max_local_id}");
    println!("  attributes      : {attrs}");
    println!("  relations       : {rels}");
    println!("  relations_items : {rel_items}");
    println!("  unique_attrs    : {unique_attrs}");
    println!("  relation_names  : {rel_names}");
    println!("  --- meshes ---");
    println!("  mesh_items      : {mesh_items}");
    println!("  samples         : {samples}");
    println!("  shells          : {shells}");
    println!("  representations : {reprs}");
    println!("  materials       : {materials}");
    println!("  local_transforms: {local_transforms}");
    println!("  global_transforms:{global_transforms}");
    println!("  material_ids    : {mat_ids}");
    println!("  representation_ids:{rep_ids}");
    println!("  sample_ids      : {sample_ids}");
    println!("  lt_ids          : {lt_ids}");
    println!("  gt_ids          : {gt_ids}");

    // Sample the first few attribute strings for entity 0
    if let Some(attr_vec) = model.attributes() {
        if attr_vec.len() > 0 {
            let first = attr_vec.get(0);
            let data = first.data();
            let preview: Vec<&str> = data.iter().take(3).collect();
            println!("  first entity attrs (up to 3): {:?}", preview);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: frag-diff <file-a.frag> <file-b.frag>");
        std::process::exit(1);
    }
    let path_a = Path::new(&args[1]);
    let path_b = Path::new(&args[2]);

    println!("Decompressing...");
    let raw_a = decompress(path_a);
    let raw_b = decompress(path_b);
    println!("  {} raw: {} bytes", path_a.file_name().unwrap().to_str().unwrap(), raw_a.len());
    println!("  {} raw: {} bytes", path_b.file_name().unwrap().to_str().unwrap(), raw_b.len());

    print_model(&format!("{}", path_a.display()), &raw_a);
    print_model(&format!("{}", path_b.display()), &raw_b);
}
