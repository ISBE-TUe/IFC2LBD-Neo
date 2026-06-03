# Testing and Benchmarking

## WASM Build — How It Works

**DO NOT** try `cargo check --target wasm32-unknown-unknown` directly — it fails without the special toolchain flags.

The only correct way to build and run the WASM frontend:

```bash
# 1. Build the .wasm + JS bindings (runs nightly cargo + wasm-bindgen + memory patch)
./scripts/build_wasm_web.sh

# 2. Run the dev server (Docker — this is what localhost:3000 uses)
cd web/wasm-prototype
docker compose up --build

# OR without Docker (requires Node.js locally):
cd web/wasm-prototype
npm ci && npm run dev
```

**What `build_wasm_web.sh` does:**

1. Compiles `crates/ifc2lbd-wasm` with `nightly` + `-Z build-std` for wasm32-unknown-unknown
2. Runs `wasm-bindgen --target web` → outputs to `web/wasm-prototype/src/wasm/`
3. Patches the generated `ifc2lbd_wasm_bg.wasm` to ensure the memory section has `shared` (required for rayon thread pool in WASM)
4. Patches a relative import path in the rayon worker helper snippet

**Outputs** (never commit these — they are gitignored):
- `web/wasm-prototype/src/wasm/ifc2lbd_wasm.js`
- `web/wasm-prototype/src/wasm/ifc2lbd_wasm_bg.wasm`
- `web/wasm-prototype/src/wasm/snippets/`

**Docker compose** (`web/wasm-prototype/docker-compose.yml`) mounts the `src/` and `public/` directories as live volumes — JS changes in `src/pipeline/` are hot-reloaded without rebuilding. Rust changes require re-running `build_wasm_web.sh` first.

**Iterating on Rust WASM code:**

```bash
# After any change to crates/ifc2lbd-wasm/ or its dependencies:
./scripts/build_wasm_web.sh
# Then reload the browser tab — no Docker restart needed (wasm file is a mounted volume)
```

**Prerequisites:** `rustup toolchain install nightly`, `wasm-bindgen-cli` (`cargo install wasm-bindgen-cli`), `wasm2wat`/`wat2wasm` (from wabt, optional — skipped if absent).

This document defines how to validate correctness and performance of `ifc2lbd-neo`.

## Test Layers

1. Unit tests (fast)
- Located in each crate.
- Validate local logic: IRI generation, property state modeling, topology merge behavior, decimal canonicalization.

2. Integration checks (medium)
- Run converter on representative IFC fixtures.
- Validate key triple patterns and expected output shape.

3. Benchmark runs (slow)
- Measure runtime and memory trends on selected fixtures.
- Compare before/after for performance-sensitive changes.

## Standard Commands

```bash
cargo test
cargo test -p lbd-converter
cargo check -p ifc2lbd-cli
python3 scripts/run_allowed_fixtures.py
python3 scripts/run_release_benchmarks.py
```

## Fixture Policy

- Keep heavy IFC fixtures out of git unless strictly required.
- Scripts should skip missing fixtures instead of hard-failing.
- Prefer a small stable set of representative fixtures for regression confidence.

## What to Verify for Converter Changes

- LBD-only mode: no topology triples unless enabled.
- IfcOWL producer active: sidecar/named-graph IfcOWL output and `owl:sameAs` links in LBD.
- `neo-topology-lite-producer`: IFC-relation topology in LBD output.
- `neo-topology-full-producer`: advanced topology mode behavior matches expectations.
- `neo-bbox-enricher` active: geometry nodes + `geo:asWKT` are emitted.
- Property/state modeling remains queryable and OPM-compatible.

## Determinism Checks

- Repeat conversion on same input and compare normalized output.
- Ensure stable ordering of emitted triples where expected.
- Keep serializer dedup behavior intact.

## Performance Checklist

When modifying hot paths (`lbd-converter`, `lbd-serializer`, topology merge):

- Run benchmark scripts before and after.
- Record wall-time deltas and memory observations.
- Note any known trade-off (for example, extra triples for better queryability).

## Minimum PR Evidence for Performance-Sensitive Changes

- Commands executed.
- Fixture(s) used.
- Before/after summary.
- Any caveats (missing fixtures, environment limits).
