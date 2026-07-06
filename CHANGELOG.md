# Changelog

All notable changes to IFC2LBD-Neo are documented in this file.

## [Unreleased]

### Added

- AGPL-3.0-only license with commercial dual-license option
- GitHub Actions CI for building CLI binaries (Linux, macOS, Windows)
- Electron desktop app with native CLI sidecar (macOS + Windows)
- Per-module stage events in CLI output (timing, triple counts, success/failure)
- GitHub Releases for distributing CLI binaries and desktop installers
- Web UI download buttons point to GitHub Releases (always latest version)

### Changed

- License: MPL-2.0 / Apache-2.0 → AGPL-3.0-only (vendored geometry stays MPL-2.0)
- Download buttons: local placeholder files → GitHub Releases URLs
- `deploy-web.yml` triggers on tag push (`v*`) in addition to `main` branch push

### Removed

- Internal plan/TODO docs (geometry, owl, plugins, structured data)
- Stale artifacts: `libnull.rlib`, `ldac_clean.svg`, `e2e-test.js`, `Dockerfile.e2e`
- Superseded `scripts/build_linux_cli.sh` (replaced by `build_all_cli.sh`)
- Placeholder CLI binaries from `web/wasm-prototype/public/bin/`

## [0.1.0] — Initial Release

### Features

- IFC STEP file parser (IFC2X3, IFC4, IFC4x3)
- LBD triple producers: BOT, BEO, Props/OPM, bSDD, OMG/FOG, IfcOWL
- Geometry pipeline: tessellation, Fragments/glTF/Parquet sidecars
- RML mapper for structured data (JSON/CSV/XML)
- Ontology mapper for external ontology alignment
- OWL reasoner
- Turtle and N-Quads (including chunked) serializers
- Plugin system with preprocess, produce, postprocess, serialize, export stages
- WebAssembly web UI with real-time pipeline visualization
- CLI with explicit module selection and configuration
- Multi-threaded conversion via rayon
