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

# ─── wasi-sdk: C/C++ toolchain targeting wasm32 ──────────────────────────────
#
# Needed only for the QTO plugin's `occt` feature. cadrum ships a prebuilt
# OpenCASCADE for wasm32-unknown-unknown, but its cxx bridge still compiles C++
# shim code for the target, and Debian's clang has no wasm sysroot or libc++.
#
# Without this the image builds everything except `occt`; with it, the same
# geometry backend runs on WASM and native, which the spike showed produces
# bit-identical results.
ARG WASI_SDK_VERSION=25
ARG WASI_SDK_BUILD=25.0
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) wasi_arch=x86_64 ;; \
      arm64) wasi_arch=arm64 ;; \
      *) echo "unsupported arch $arch" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/wasi-sdk.tar.gz \
      "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${WASI_SDK_VERSION}/wasi-sdk-${WASI_SDK_BUILD}-${wasi_arch}-linux.tar.gz"; \
    mkdir -p /opt/wasi-sdk; \
    tar -xzf /tmp/wasi-sdk.tar.gz -C /opt/wasi-sdk --strip-components=1; \
    rm /tmp/wasi-sdk.tar.gz

# cc-rs picks these up for the wasm32 target.
ENV WASI_SDK_PATH=/opt/wasi-sdk
ENV CC_wasm32_unknown_unknown=/opt/wasi-sdk/bin/clang \
    CXX_wasm32_unknown_unknown=/opt/wasi-sdk/bin/clang++ \
    AR_wasm32_unknown_unknown=/opt/wasi-sdk/bin/llvm-ar

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
