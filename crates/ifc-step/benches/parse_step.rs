use std::fs;
use std::path::{Path, PathBuf};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ifc_step::{parse_step_bytes, parse_step_file};

fn bench_parse_step(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_step");

    for path in benchmark_files() {
        let bytes = fs::read(&path).expect("failed to read benchmark IFC file");
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ifc");

        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("parse_step_bytes", label),
            &bytes,
            |b, data| {
                b.iter(|| parse_step_bytes(data).expect("parse_step_bytes failed"));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("parse_step_file", label),
            &path,
            |b, path| {
                b.iter(|| parse_step_file(path).expect("parse_step_file failed"));
            },
        );
    }

    group.finish();
}

fn benchmark_files() -> Vec<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("../..");
    let mut files = vec![repo_root.join("Duplex.ifc")];

    let large_fixture = repo_root.join("CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc");
    if large_fixture.exists() {
        files.push(large_fixture);
    }

    files
}

criterion_group!(benches, bench_parse_step);
criterion_main!(benches);
