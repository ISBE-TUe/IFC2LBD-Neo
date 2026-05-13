# ─── Build environment for ifc2lbd WASM ──────────────────────────────────────
#
# What this image provides:
#   • Rust nightly  (required for -Z build-std)
#   • wasm32-unknown-unknown target
#   • rust-src component  (required for -Z build-std=std,panic_abort)
#   • wasm-bindgen-cli pinned to the same version used in Cargo.lock (0.2.118)
#
# Usage (via docker compose — see docker-compose.yml):
#   docker compose run --rm check          # fast type-check, no output files
#   docker compose run --rm build          # full release build → web/wasm-prototype/src/wasm/
#
# The image itself is only rebuilt when this file changes.
# Rust registry and the target/ directory are cached in named Docker volumes,
# so incremental rebuilds inside the container are fast.

FROM rust:latest

# System packages needed by some Cargo dependencies (ring, openssl, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    wabt \
    && rm -rf /var/lib/apt/lists/*

# Install nightly toolchain, the wasm32 target, and rust-src (for build-std)
RUN rustup toolchain install nightly \
    && rustup target add wasm32-unknown-unknown --toolchain nightly \
    && rustup component add rust-src --toolchain nightly

# Pin wasm-bindgen-cli to match Cargo.lock — mismatch causes a hard error
RUN cargo +nightly install wasm-bindgen-cli --version 0.2.118 --locked

# Make nightly the active toolchain inside the container so callers don't need +nightly
ENV RUSTUP_TOOLCHAIN=nightly

WORKDIR /workspace
