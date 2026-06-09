#!/usr/bin/env bash
# Build the ifc2lbd-neo CLI as a Linux x86_64 binary using Docker.
#
# Why Docker (and not `cross`)?
#   This repo has a rustup *directory override* to nightly (needed for the WASM
#   `build-std` step). `cross` reads that override and then tries to install a
#   non-host toolchain (e.g. `stable-x86_64-unknown-linux-gnu`) on the macOS
#   host, which modern rustup refuses without `--force-non-host`. Building
#   inside a plain Linux `rust` container sidesteps all of that.
#
# Output:
#   ./ifc2lbd-neo-linux-x86_64                              (copied to repo root)
#   ./target/linux-x86_64/release/ifc2lbd-neo              (cargo target dir)
#
# Speed:
#   - A named volume caches the cargo registry, so deps are downloaded only once.
#   - The target dir (target/linux-x86_64) is reused, so re-runs are incremental.
#   - On Apple Silicon this runs under x86_64 emulation: ~9 min cold, fast warm.
#     Do NOT wipe target/linux-x86_64 between runs unless you want a full rebuild.
#   - Don't launch a second build into the same target dir while one is running;
#     concurrent cargo runs corrupt the dir.
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"

docker run --rm --platform linux/amd64 \
  -v "$REPO":/work -w /work \
  -v ifc2lbd_cargo_registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/linux-x86_64 \
  rust:latest \
  bash -c '
    set -euo pipefail
    # System deps for native build dependencies:
    #   clang  -> zstd-sys (parquet)        cmake -> manifold-csg-sys (geometry)
    #   pkg-config + libssl-dev -> ring/TLS crates
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends pkg-config libssl-dev clang cmake >/dev/null
    cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
  '

cp "target/linux-x86_64/release/ifc2lbd-neo" "ifc2lbd-neo-linux-x86_64"
echo "Built: ifc2lbd-neo-linux-x86_64"
file "ifc2lbd-neo-linux-x86_64"
