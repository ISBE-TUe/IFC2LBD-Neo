# worker-rml-rust: High-performance RML worker using Rust
#
# Build context must include both:
#   - workers/worker-rml-rust/ (this package)
#   - rust_rml_mapper/ (the library, copied from sibling repo)
#
# Build from monorepo root:
#   docker build -f workers/worker-rml-rust/Dockerfile \
#     --build-context rml_mapper=../rust_rml_mapper \
#     -t ontocore-worker-rml-rust .
#
# Or use the provided build script:
#   ./workers/worker-rml-rust/build.sh

# ═══════════════════════════════════════════════════════════════
# Stage 1: Build
# ═══════════════════════════════════════════════════════════════
FROM rust:latest AS builder

WORKDIR /build

# Install build dependencies (including clang for bindgen/rocksdb)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    clang \
    libclang-dev \
    cmake \
    make \
    g++ \
    && rm -rf /var/lib/apt/lists/*

# Copy rml_mapper library source (from additional build context or pre-copied)
COPY rml_mapper/ /build/rml_mapper/

# Copy worker-rml-rust source
COPY Cargo.toml /build/worker/Cargo.toml
COPY src/ /build/worker/src/

# Fix Cargo.toml path for Docker build context
RUN sed -i 's|path = "../../../rust_rml_mapper"|path = "../rml_mapper"|' /build/worker/Cargo.toml

WORKDIR /build/worker

# Pin home crate to stable version (0.5.11 works with Rust 1.85+)
# This avoids the home@0.5.12 which requires Rust 1.88 (unreleased)
RUN cargo update home@0.5.12 --precise 0.5.11 2>/dev/null || true

# Build release binary
RUN cargo build --release

# ═══════════════════════════════════════════════════════════════
# Stage 2: Runtime
# ═══════════════════════════════════════════════════════════════
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /build/worker/target/release/worker-rml-rust /usr/local/bin/worker-rml-rust

# Create non-root user
RUN useradd -m -s /bin/bash rml
USER rml

WORKDIR /data

# Default environment
ENV PORT=8000 \
    LOG_LEVEL=info \
    RUST_BACKTRACE=1

EXPOSE 8000

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8000/healthz || exit 1

ENTRYPOINT ["worker-rml-rust"]
