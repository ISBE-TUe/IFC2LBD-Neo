use std::path::PathBuf;

use flatbuffers_build::BuilderOptions;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let schema = manifest_dir.join("schema").join("fragments.fbs");

    BuilderOptions::new_with_files([schema])
        .compile()
        .expect("failed to compile fragments flatbuffer schema");
}
