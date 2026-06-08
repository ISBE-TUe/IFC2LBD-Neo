use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use crossbeam::channel;
use ifc_model::build_model;
use ifc_step::parse_step_file;
use lbd_converter::{
    stream_bot, stream_beo, ConvertOptions,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn test_ifc_path() -> PathBuf {
    workspace_root().join("web/wasm-prototype/public/sample.ifc")
}

fn small_ifc_path() -> PathBuf {
    workspace_root().join("web/wasm-prototype/public/test.ifc")
}

fn bench_parse_and_model(c: &mut Criterion) {
    let path = small_ifc_path();
    if !path.exists() {
        eprintln!("Skipping bench_parse_and_model: {:?} not found", path);
        return;
    }
    c.bench_function("parse_step + build_model (test.ifc)", |b| {
        b.iter(|| {
            let step = parse_step_file(&path).expect("parse");
            let _model = build_model(&step).expect("build");
        })
    });
}

fn bench_bot_producer(c: &mut Criterion) {
    let path = small_ifc_path();
    if !path.exists() {
        eprintln!("Skipping bench_bot_producer: {:?} not found", path);
        return;
    }
    let step = parse_step_file(&path).expect("parse");
    let model = build_model(&step).expect("build");
    let options = ConvertOptions::default();

    c.bench_function("stream_bot (test.ifc)", |b| {
        b.iter(|| {
            let (tx, rx) = channel::unbounded();
            stream_bot(&model, &options, &tx).expect("stream_bot");
            drop(tx);
            let count: usize = rx.into_iter().map(|batch| batch.len()).sum();
            assert!(count > 0);
        })
    });
}

fn bench_bot_beo_producers(c: &mut Criterion) {
    let path = small_ifc_path();
    if !path.exists() {
        eprintln!("Skipping bench_bot_beo_producers: {:?} not found", path);
        return;
    }
    let step = parse_step_file(&path).expect("parse");
    let model = build_model(&step).expect("build");
    let options = ConvertOptions::default();

    c.bench_function("stream_bot + stream_beo sequential (test.ifc)", |b| {
        b.iter(|| {
            let mut total = 0usize;
            {
                let (tx, rx) = channel::unbounded();
                stream_bot(&model, &options, &tx).expect("stream_bot");
                drop(tx);
                total += rx.into_iter().map(|batch| batch.len()).sum::<usize>();
            }
            {
                let (tx, rx) = channel::unbounded();
                stream_beo(&model, &options, &tx).expect("stream_beo");
                drop(tx);
                total += rx.into_iter().map(|batch| batch.len()).sum::<usize>();
            }
            assert!(total > 0);
        })
    });
}

fn bench_digitalhub_parse(c: &mut Criterion) {
    let path = test_ifc_path();
    if !path.exists() {
        eprintln!("Skipping bench_digitalhub_parse: {:?} not found", path);
        return;
    }
    c.bench_function("parse_step + build_model (sample.ifc)", |b| {
        b.iter(|| {
            let step = parse_step_file(&path).expect("parse");
            let _model = build_model(&step).expect("build");
        })
    });
}

fn bench_digitalhub_bot(c: &mut Criterion) {
    let path = test_ifc_path();
    if !path.exists() {
        eprintln!("Skipping bench_digitalhub_bot: {:?} not found", path);
        return;
    }
    let step = parse_step_file(&path).expect("parse");
    let model = build_model(&step).expect("build");
    let options = ConvertOptions::default();

    c.bench_function("stream_bot (sample.ifc)", |b| {
        b.iter(|| {
            let (tx, rx) = channel::unbounded();
            stream_bot(&model, &options, &tx).expect("stream_bot");
            drop(tx);
            let count: usize = rx.into_iter().map(|batch| batch.len()).sum();
            assert!(count > 0);
        })
    });
}

criterion_group!(
    benches,
    bench_parse_and_model,
    bench_bot_producer,
    bench_bot_beo_producers,
    bench_digitalhub_parse,
    bench_digitalhub_bot,
);
criterion_main!(benches);
