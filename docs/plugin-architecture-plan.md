# Plugin Architecture Plan

## Why

The current code already has the right pressure points for a staged pipeline:

- parse/build model
- production of LBD / IfcOWL / topology data
- serialization to Turtle or N-Quads
- export sinks such as files or direct Grafeo streaming

What is missing is a stable extension boundary. Right now new behavior such as Grafeo direct streaming or voxel output gets wired into the CLI and core crates directly. That works for experiments, but it does not scale if we want users or AI agents to add new producers or exporters.

The next architecture step should therefore be:

1. make the pipeline stages explicit
2. define crate-level plugin traits for those stages
3. keep plugin registration simple and deterministic
4. make the design compatible with a future WASM packaging model

## Recommendation

Use a compile-time crate plugin system first, not runtime dynamic loading.

That means:

- each plugin is a normal Rust crate in the workspace
- each plugin implements one or more stage traits
- the CLI or runtime crate builds a registry from enabled plugins
- plugins are selected by config / CLI flags

This is the right first step because:

- it keeps type safety
- it works well with Rust tooling and CI
- it is easier to debug than dynamic loading
- it is compatible with later WASM/component packaging

Do not start with `dlopen`-style dynamic libraries. That path will fight Rust ergonomics and becomes worse once WASM enters the picture.

## Target Stage Model

Define an explicit pipeline with four extension points:

1. `preprocess`
   - enrich or index IFC/model data before production
   - examples: GUID normalization cache, geometry pre-index, property filtering, spatial index

2. `produce`
   - produce semantic records from the model
   - examples: LBD producer, IfcOWL producer, topology producer, voxel producer

3. `serialize`
   - encode records into a transport syntax
   - examples: Turtle, N-Quads, TriG, JSON stream, binary framed stream

4. `export`
   - send serialized output to a sink
   - examples: files, stdout, Grafeo store writer, HTTP sink, chunked object storage

This keeps semantic generation separate from output transport. That separation matters for WASM later.

## Core Design

Add a new crate, for example `crates/lbd-pipeline`, that defines:

- shared pipeline data types
- plugin traits
- stage context
- registry and execution plan

Suggested core traits:

```rust
pub trait PipelinePlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn manifest(&self) -> PluginManifest;
}

pub trait PreprocessPlugin: PipelinePlugin {
    fn run(&self, ctx: &mut PipelineContext) -> anyhow::Result<()>;
}

pub trait ProducerPlugin: PipelinePlugin {
    fn produces(&self) -> &'static [RecordKind];
    fn run(&self, ctx: &PipelineContext, out: &dyn RecordSink) -> anyhow::Result<()>;
}

pub trait SerializerPlugin: PipelinePlugin {
    fn input_kind(&self) -> RecordKind;
    fn output_kind(&self) -> StreamKind;
    fn run(&self, input: &dyn RecordStream, out: &mut dyn ByteSink) -> anyhow::Result<()>;
}

pub trait ExportPlugin: PipelinePlugin {
    fn input_kind(&self) -> StreamKind;
    fn run(&self, input: &mut dyn ByteStream, ctx: &ExportContext) -> anyhow::Result<()>;
}
```

The important point is that producers should emit a typed intermediate stream, not write files directly.

## Data Boundary

The main architectural decision is the record boundary between `produce` and `serialize`.

Use a small internal IR instead of exposing ontology internals everywhere.

Suggested initial IR:

- `Record::Triple(TripleRecord)`
- `Record::Quad(QuadRecord)`
- `Record::Geometry(GeometryRecord)`
- `Record::Diagnostic(DiagnosticRecord)`

For the first version, even a triple/quad-focused IR is enough. The point is to stop hard-coding "producer also decides final file layout".

That lets you support:

- LBD producer -> Turtle serializer -> file exporter
- IfcOWL producer -> N-Quads serializer -> Grafeo exporter
- voxel producer -> custom serializer -> robot sink

## Registry Model

Keep registration explicit.

Recommended first version:

- a `registry` module with a static list of available plugins
- each plugin crate exports `pub fn register(registry: &mut Registry)`
- the CLI builds a registry from enabled workspace features

Example:

```rust
#[cfg(feature = "plugin-grafeo-export")]
grafeo_export_plugin::register(&mut registry);

#[cfg(feature = "plugin-voxel-producer")]
voxel_producer_plugin::register(&mut registry);
```

This is boring, but it is robust and easy for AI tooling to generate correctly.

## Plugin Manifest

Each plugin should provide metadata used by the CLI, docs, and future template tooling.

Suggested manifest fields:

- `id`
- `display_name`
- `version`
- `stage`
- `description`
- `inputs`
- `outputs`
- `config_schema`
- `parallelism`
- `wasm_compatible`

This lets the runtime validate pipeline assembly before execution.

## Parallel Execution Model

Parallelism should live in the pipeline runner, not be reinvented per plugin.

Recommended model:

- each stage declares whether it is:
  - `serial`
  - `parallel_by_entity`
  - `parallel_by_batch`
  - `parallel_by_partition`
- the runner owns the worker pools and channels
- plugins declare capabilities and preferred batch sizes

That avoids plugins each spawning uncontrolled thread trees.

For example:

- IfcOWL producer: `parallel_by_partition`
- topology producer: `parallel_by_entity` or `parallel_by_partition`
- serializer: usually `serial` per stream, but multiple serializers can run in parallel on different streams
- exporter: often `serial` for one sink, but multiple exporters may consume cloned streams if configured

## Grafeo Streaming

Your current Grafeo stream path should become an `export` plugin, not a CLI special case.

Meaning:

- producer emits quads
- N-Quads serializer or framed-stream serializer converts them
- Grafeo exporter consumes the serialized or framed stream

That is a cleaner fit than keeping `--grafeo-direct-stream` inside `main.rs`.

So the target is:

- move the framing protocol and Grafeo-specific transport into `plugin-grafeo-export`
- keep the generic RDF production in core crates

## Voxel Example

The voxel example is exactly why this architecture is worth doing.

A future `plugin-voxel-producer` should be able to:

- consume IFC model plus optional geometry cache from preprocess stage
- emit `GeometryRecord::VoxelGrid` or similar
- feed either:
  - a voxel serializer/exporter pair
  - or a hybrid producer that also emits RDF summaries

That means a user can add "robot voxels" without modifying the base LBD or IfcOWL producers.

## WASM Direction

WASM should be treated as a packaging target and runtime boundary, not as the first plugin mechanism.

Recommended order:

1. native crate plugin architecture
2. pipeline IR cleanup
3. WASM-safe interfaces
4. optional WASM component packaging for selected plugins

Important constraint:

- runtime loading of arbitrary Rust crates is not the same thing as running in WASM
- for WASM you will likely want a stable ABI, likely via WIT/WASI component interfaces

So the architecture should assume:

- plugins may be compiled into the binary in native mode
- some plugins may later be compiled as WASM components if they only use approved interfaces

## Native vs WASM Plugin Split

Add a capability flag per plugin:

- `native_only`
- `wasm_compatible`
- `requires_subprocess`
- `requires_filesystem`
- `requires_network`

For example:

- OCC exact geometry plugin: probably `native_only`
- pure RDF producer: likely `wasm_compatible`
- Grafeo HTTP exporter: `wasm_compatible` if the host provides network access

This avoids promising that every plugin can run in every packaging target.

## Template Strategy

Create a plugin template generator for AI-assisted extension work.

The template should generate:

- `Cargo.toml`
- `src/lib.rs`
- plugin manifest
- config struct
- stage trait implementation
- smoke test
- README with "how to enable"

Template families:

- producer template
- serializer template
- export template
- preprocess template

For an AI workflow, the template should be minimal and opinionated. The AI should fill in one stage, one record type, one config object, one test.

## Suggested Repo Shape

One reasonable workspace layout:

```text
crates/
  lbd-pipeline/
  lbd-runtime/
  plugin-lbd-producer/
  plugin-ifcowl-producer/
  plugin-topology-producer/
  plugin-grafeo-export/
  plugin-voxel-producer/
  plugin-turtle-serializer/
  plugin-nquads-serializer/
```

Where:

- `lbd-runtime` assembles and runs pipelines
- the CLI becomes a thin frontend over `lbd-runtime`
- existing crates such as `lbd-converter` and `lbd-serializer` get reused internally by plugins

## Migration Plan

### Phase 1

- introduce `lbd-pipeline`
- define stage traits and registry
- keep current CLI behavior unchanged
- wrap existing LBD / IfcOWL / topology logic as internal built-in plugins

### Phase 2

- move Grafeo direct stream out of CLI into an export plugin
- split serializers into plugin form
- keep file/stdout export as built-in exporters

### Phase 3

- extract voxel logic into a producer plugin
- add plugin templates
- add manifest-driven plugin listing in CLI

### Phase 4

- define WASM-safe plugin boundary
- evaluate WASI component model for selected plugins
- package a reduced converter runtime for browser/serverless use

## What Not To Do

- do not let every plugin define its own threading model
- do not let producers write files directly unless they are explicitly export plugins
- do not start with dynamic loading as the primary mechanism
- do not couple plugin identity to CLI flags only
- do not promise that OCC or heavy native geometry code will be WASM-ready immediately

## Immediate Next Steps

1. extract a new `lbd-pipeline` crate with stage traits and registry
2. move Grafeo direct streaming behind an exporter plugin boundary
3. wrap current LBD / IfcOWL / topology code as built-in producer plugins
4. add one example external plugin crate, ideally `plugin-voxel-producer`
5. add a plugin template scaffold so AI can generate future crates consistently
6. only then start the WASM packaging track

## Bottom Line

The idea is good, but it should be implemented as a staged crate-plugin architecture first, with compile-time registration and a stable internal record boundary.

That gives you:

- easier parallel pipelines
- a clean path for Grafeo streaming
- a realistic AI-generated plugin workflow
- a much better foundation for later WASM packaging
