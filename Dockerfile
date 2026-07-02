# ─── Build environment for ifc2lbd WASM ──────────────────────────────────────
#
# What this image provides:
#   • Rust nightly  (required for -Z build-std)
#   • wasm32-unknown-unknown target
#   • rust-src component  (required for -Z build-std=std,panic_abort)
#   • wasm-bindgen-cli from patched vendor/ (0.2.126 with wasm64 + ABI fixes)
#
# Usage (via docker compose — see docker-compose.yml):
#   docker compose run --rm check          # fast type-check, no output files
#   docker compose run --rm build          # full release build → web/wasm-prototype/src/wasm/
#
# The image itself is only rebuilt when this file changes.
# Rust registry and the target/ directory are cached in named Docker volumes,
# so incremental rebuilds inside the container are fast.

FROM rust:latest

# System packages needed by Cargo dependencies
# - clang: required by zstd-sys (parquet Snappy/Zstd compression)
# - libssl-dev, pkg-config: required by ring and other TLS crates
# - wabt: wasm2wat/wat2wasm for post-processing the wasm binary
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    wabt \
    clang \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Install nightly toolchain, the wasm32 target, and rust-src (for build-std)
RUN rustup toolchain install nightly \
    && rustup target add wasm32-unknown-unknown --toolchain nightly \
    && rustup component add rust-src --toolchain nightly

# Install wasm-bindgen-cli from the patched vendor copy (0.2.126 with
# wasm64 threading + f64/BigInt ABI fixes).  Mismatch with the wasm-bindgen
# runtime version in Cargo.lock causes a hard error.
COPY vendor/wasm-bindgen-cli-0.2.126 /vendor/wasm-bindgen-cli-0.2.126
COPY vendor/wasm-bindgen-cli-support-0.2.126 /vendor/wasm-bindgen-cli-support-0.2.126
RUN cargo +nightly install --path /vendor/wasm-bindgen-cli-0.2.126 --force

# Make nightly the active toolchain inside the container so callers don't need +nightly
ENV RUSTUP_TOOLCHAIN=nightly

WORKDIR /workspace
