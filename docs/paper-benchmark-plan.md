# Paper Benchmark Plan

This document defines the benchmark campaign for Section 4 of the EG-ICE paper.
It is intentionally narrow. The goal is not to benchmark every flag combination, but
to generate one clear baseline comparison, one clear scalability study, and one clear
geometry-quality case.

Naming rule for the paper:
- use output-oriented labels in tables and figures
- do not use shorthand such as `L3` in visible paper assets
- if the Java CLI uses `--level 3`, mention that in running text or captions, not in the row labels

## Evaluation Order

1. `model-A.ifc`
   - direct Java vs Rust baseline
   - Rust mode comparison
   - IfcOWL completeness check
2. `CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc`
   - large-model scalability
   - chunked N-Quads behavior
   - 1 vs 4 vs 16 core scaling
3. `Wohn-Geschaeftshaus.ifc`
   - bbox escalation behavior
   - diagonal and rounded element case

## Hardware Target

- OS: Debian 12
- CPU: AMD 9955HX
- RAM: 64 GB
- Core-count study:
  - 1 core
  - 4 cores
  - 16 cores

## Measurement Method

- Use release builds only.
- Run the model A benchmark as a repeated experiment, not as a single-shot measurement.
- Default repeat count for the paper: 5 runs per model A configuration.
- Measure runtime and memory with `/usr/bin/time -v`.
- Report mean and sample standard deviation for runtime and peak RSS.
- Record exact command lines in the benchmark notes.
- Record nonzero exits explicitly.
- For large-model runs, record free disk space before and after the run.

## Paths And Variables

Adjust these once on the benchmark server, then keep them fixed for the campaign.

```bash
ROOT=/path/to/ifc2lbd-neo
RUST_BIN="$ROOT/target/release/ifc2lbd-neo"
COMPARE_BIN="$ROOT/target/release/compare-turtle"
OUT="$ROOT/artifacts/paper-benchmarks-server"

JAVA_MAIN="org.linkedbuildingdata.ifc2lbd.IFCtoLBDConverter_CLI"
JAVA_CP="/path/to/IFCtoLBD_Python/jars/ifc-to-lbd-2.44.0.jar:/path/to/IFCtoLBD_Python/jars/*"

DIGITALHUB="$ROOT/model-A.ifc"
LARGE="$ROOT/CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc"
WOHN="$ROOT/Wohn-Geschaeftshaus.ifc"

BASE_DH="https://benchmark.test/digitalhub/"
BASE_LARGE="https://benchmark.test/coord/"
BASE_WOHN="https://benchmark.test/wohn/"

CORE1="taskset -c 0"
CORE4="taskset -c 0-3"
CORE16="taskset -c 0-15"
```

## Build Step

```bash
cd "$ROOT"
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo --bin compare-turtle
mkdir -p "$OUT"
```

## Benchmark Matrix

### 1. model A Baseline

Purpose:
- direct Rust vs Java runtime comparison
- peak RSS comparison
- IfcOWL completeness baseline
- LBD comparison with explicit note that the LBD surface is not a strict 1:1 oracle
- repeated-run statistics for the paper table and plots

Recommended runner:

```bash
python3 scripts/paper_benchmark_digitalhub.py \
  --repeats 5 \
  --out-dir "$OUT/digitalhub_repeated"
```

The five model A configurations should appear in the paper with these labels:
- `Java LBD + IfcOWL (TTL)`
- `Rust LBD + IfcOWL (TTL)`
- `Rust LBD + IfcOWL (N-Quads chunked)`
- `Rust LBD + IfcOWL + topology`
- `Rust LBD + IfcOWL + full topology + bboxes`

#### 1.1 Rust Baseline

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$DIGITALHUB" \
  --output "$OUT/digitalhub_rust_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_DH"
```

#### 1.2 Java Baseline

```bash
/usr/bin/time -v \
  java -Xms256m -Xmx16G -cp "$JAVA_CP" "$JAVA_MAIN" \
  "$DIGITALHUB" \
  --url "$BASE_DH" \
  --level 3 \
  --target_file "$OUT/digitalhub_java_lbd.ttl" \
  --hasBuildingElements \
  --hasBuildingElementProperties \
  --hasUnits \
  --hasGeolocation \
  --ifcOWL
```

#### 1.3 model A Comparison Checks

LBD compare:

```bash
"$COMPARE_BIN" \
  "$OUT/digitalhub_java_lbd.ttl" \
  "$OUT/digitalhub_rust_lbd.ttl" \
  --left-base "$BASE_DH" \
  --right-base "$BASE_DH" \
  --normalize-lbd-opm
```

IfcOWL completeness:
- record output file sizes
- optionally record line counts
- note any Java-side export warnings or errors

Recommended paper assets from the repeated model A run:
- one table with mean ± sd for:
  - Java LBD + IfcOWL (TTL)
  - Rust LBD + IfcOWL (TTL)
  - Rust LBD + IfcOWL (N-Quads chunked)
  - Rust LBD + IfcOWL + topology
  - Rust LBD + IfcOWL + full topology + bboxes
- one baseline comparison plot with error bars for:
  - wall time
  - peak RSS
  - output size
- one Rust mode comparison plot with error bars for:
  - wall time
  - peak RSS
  - output size

### 2. model A Rust Mode Comparison

Purpose:
- compare plain Turtle against chunked N-Quads
- quantify the overhead of `--topology`
- quantify the overhead of `--topology-full --bbox`

#### 2.1 Plain Turtle + IfcOWL

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$DIGITALHUB" \
  --output "$OUT/digitalhub_rust_ttl_mode_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_DH"
```

#### 2.2 Chunked N-Quads + IfcOWL

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$DIGITALHUB" \
  --output-format nquads \
  --output "$OUT/digitalhub_mode_nq/out.nq" \
  --base-uri "$BASE_DH" \
  --ifcowl \
  --quad-chunking cores \
  --quad-chunk-prefix digitalhub
```

#### 2.3 Lightweight Topology

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$DIGITALHUB" \
  --output "$OUT/digitalhub_rust_topology_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_DH" \
  --topology
```

#### 2.4 Full Topology + BBox

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$DIGITALHUB" \
  --output "$OUT/digitalhub_rust_topology_full_bbox_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_DH" \
  --topology-full \
  --bbox
```

### 3. Large-Model Scalability

Purpose:
- show scaling behavior on a high-output workload
- test 1 vs 4 vs 16 cores
- test chunked N-Quads as the main scalable output path
- quantify chunking overhead relative to plain Turtle if feasible

#### 3.1 Chunked N-Quads, 1 Core

```bash
/usr/bin/time -v \
  $CORE1 "$RUST_BIN" "$LARGE" \
  --output-format nquads \
  --output "$OUT/large_1core_nq/out.nq" \
  --base-uri "$BASE_LARGE" \
  --ifcowl \
  --quad-chunking cores \
  --quad-chunk-prefix large
```

#### 3.2 Chunked N-Quads, 4 Cores

```bash
/usr/bin/time -v \
  $CORE4 "$RUST_BIN" "$LARGE" \
  --output-format nquads \
  --output "$OUT/large_4core_nq/out.nq" \
  --base-uri "$BASE_LARGE" \
  --ifcowl \
  --quad-chunking cores \
  --quad-chunk-prefix large
```

#### 3.3 Chunked N-Quads, 16 Cores

```bash
/usr/bin/time -v \
  $CORE16 "$RUST_BIN" "$LARGE" \
  --output-format nquads \
  --output "$OUT/large_16core_nq/out.nq" \
  --base-uri "$BASE_LARGE" \
  --ifcowl \
  --quad-chunking cores \
  --quad-chunk-prefix large
```

#### 3.4 Optional Plain Turtle Reference

Run this only if disk space and wall time are acceptable.

```bash
/usr/bin/time -v \
  $CORE16 "$RUST_BIN" "$LARGE" \
  --output "$OUT/large_16core_ttl_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_LARGE"
```

### 4. Wohn-Geschaeftshaus Geometry Case

Purpose:
- verify that diagonal and rounded elements trigger the bbox escalation path
- quantify how many elements actually need exact handling

#### 4.1 BBox Report Run

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$WOHN" \
  --output "$OUT/wohn_bbox_lbd.ttl" \
  --base-uri "$BASE_WOHN" \
  --bbox \
  --bbox-report "$OUT/wohn_bbox_report.json"
```

#### 4.2 Optional Full Topology Run

```bash
/usr/bin/time -v \
  "$RUST_BIN" "$WOHN" \
  --output "$OUT/wohn_topology_full_lbd.ttl" \
  --ifcowl \
  --base-uri "$BASE_WOHN" \
  --topology-full \
  --bbox
```

## What To Record

For every run:
- command line
- converter version or commit
- model name and file size
- wall time
- user time
- sys time
- max resident set size
- return code
- total output size

For model A:
- Rust vs Java speedup
- Rust vs Java RSS ratio
- normalized LBD compare result
- note on IfcOWL completeness and any Java export warnings

For model A Rust modes:
- plain Turtle runtime
- chunked N-Quads runtime
- `--topology` overhead relative to plain Turtle
- `--topology-full --bbox` overhead relative to plain Turtle

For large-model runs:
- 1 vs 4 vs 16 core runtime
- output size per graph family
- number of chunk files produced
- free disk before and after
- whether the run finalized cleanly

For Wohn-Geschaeftshaus:
- `elements_requested`
- `elements_with_mesh`
- `escalated_exact_count`
- `rotated_bbox_count`
- `avg_inflation_fast`
- `max_inflation_fast`
- `avg_inflation_final`
- `max_inflation_final`
- top outliers

## Expected Section Mapping

- `4.4 Performance Comparison`
  - model A Java vs Rust baseline
- `4.5 Mode Comparison`
  - model A Rust mode runs
- `4.6 Scalability Evaluation`
  - large-model 1/4/16 core runs
- `4.7 Topology and Bounding-Box Observations`
  - Wohn bbox report

## Paper Notes

- Treat model A IfcOWL as the closest completeness comparison.
- State explicitly that the model A LBD surface is not a strict 1:1 oracle.
- Keep the large-model section Rust-only unless a Java run becomes realistic on the server.
- If a large-model run ends because of disk pressure, report it as an environment limit, not as a semantic failure.
- Use desaturated red and grey tones for paper plots.
- Prefer error bars over decorative plot styling.
