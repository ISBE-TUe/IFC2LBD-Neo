#!/usr/bin/env bash
# Build ifc2lbd-neo CLI binaries for Linux, macOS, and Windows.
#
# Platform strategy:
#   Linux   → Docker (linux/amd64)   — same as existing build_linux_cli.sh
#   macOS   → Native (aarch64-apple-darwin) — Docker can't produce macOS binaries
#   Windows → Docker (linux/amd64, x86_64-pc-windows-gnu) — MinGW cross-compile
#
# Outputs (repo root):
#   ./ifc2lbd-neo-linux-x86_64
#   ./ifc2lbd-neo-macos
#   ./ifc2lbd-neo-windows.exe
#
# Then copies them to web/wasm-prototype/public/bin/ and dist/bin/
# (served by the web UI download buttons).
#
# Requirements:
#   - Docker Desktop (with QEMU for linux/amd64 emulation on Apple Silicon)
#   - Rust toolchain installed locally (for macOS native build)
#   - ~15-20 GB disk for Docker volumes (cargo registry + target dirs)

set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
BIN_DIR="$REPO/web/wasm-prototype/public/bin"
DIST_DIR="$REPO/web/wasm-prototype/dist/bin"

# ─── Helpers ─────────────────────────────────────────────────────────────────

log() { echo -e "\n\033[1;32m>>> $*\033[0m"; }
warn() { echo -e "\033[1;33m!! $*\033[0m"; }
die()  { echo -e "\033[1;31m✗ $*\033[0m"; exit 1; }

ensure_docker() {
  command -v docker >/dev/null 2>&1 || die "Docker is required. Install Docker Desktop."
  docker info >/dev/null 2>&1 || die "Docker is not running."
}

ensure_rust() {
  command -v cargo >/dev/null 2>&1 || die "Rust toolchain required for macOS native build."
}

# ─── Linux (x86_64) via Docker ───────────────────────────────────────────────

build_linux() {
  log "Building Linux x86_64 binary (Docker, linux/amd64)..."

  docker run --rm --platform linux/amd64 \
    -v "$REPO":/work -w /work \
    -v ifc2lbd_cargo_registry:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/work/target/linux-x86_64 \
    rust:latest \
    bash -c '
      set -euo pipefail
      apt-get update -qq
      apt-get install -y -qq --no-install-recommends pkg-config libssl-dev clang cmake >/dev/null
      cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
    '

  cp "target/linux-x86_64/release/ifc2lbd-neo" "$REPO/ifc2lbd-neo-linux-x86_64"
  chmod +x "$REPO/ifc2lbd-neo-linux-x86_64"
  log "✓ Linux: $(du -h "$REPO/ifc2lbd-neo-linux-x86_64" | cut -f1)"
  file "$REPO/ifc2lbd-neo-linux-x86_64"
}

# ─── macOS (aarch64) — Native build ──────────────────────────────────────────
#
# Docker cannot produce macOS binaries (requires Apple signed code signing
# entitlements and the Apple SDK). We build natively on the host.

build_macos() {
  log "Building macOS aarch64 binary (native)..."

  cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo --target aarch64-apple-darwin

  cp "target/aarch64-apple-darwin/release/ifc2lbd-neo" "$REPO/ifc2lbd-neo-macos"
  chmod +x "$REPO/ifc2lbd-neo-macos"
  log "✓ macOS: $(du -h "$REPO/ifc2lbd-neo-macos" | cut -f1)"
  file "$REPO/ifc2lbd-neo-macos"
}

# ─── Windows (x86_64) via Docker ─────────────────────────────────────────────

build_windows() {
  log "Building Windows x86_64 binary (Docker, x86_64-pc-windows-gnu)..."

  docker run --rm --platform linux/amd64 \
    -v "$REPO":/work -w /work \
    -v ifc2lbd_cargo_registry:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/work/target/x86_64-pc-windows-gnu \
    -e RUSTFLAGS='--cfg getrandom_backend="windows_legacy"' \
    rust:latest \
    bash -c '
      set -euo pipefail
      # Add Windows cross-compile target
      rustup target add x86_64-pc-windows-gnu
      # Install system deps
      apt-get update -qq
      apt-get install -y -qq --no-install-recommends pkg-config libssl-dev clang cmake >/dev/null
      # Build for Windows GNU target (produces .exe via MinGW-w64)
      # RUSTFLAGS forces getrandom to use the windows_legacy backend (advapi32.dll)
      # instead of the default windows backend which requires bcryptprimitives.dll
      cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo \
        --target x86_64-pc-windows-gnu
    '

  cp "target/x86_64-pc-windows-gnu/release/ifc2lbd-neo.exe" "$REPO/ifc2lbd-neo-windows.exe"
  log "✓ Windows: $(du -h "$REPO/ifc2lbd-neo-windows.exe" | cut -f1)"
  file "$REPO/ifc2lbd-neo-windows.exe"
}

# ─── Copy to web download directories ────────────────────────────────────────

deploy_binaries() {
  log "Binaries are now published via GitHub Releases."
  log "Local copies remain in the repo root for testing."
  ls -lh "$REPO/ifc2lbd-neo-linux-x86_64" "$REPO/ifc2lbd-neo-macos" 2>/dev/null
}

# ─── Main ────────────────────────────────────────────────────────────────────

ensure_docker
ensure_rust

# Check Docker platform support
if [[ "$(uname -m)" == "arm64" ]]; then
  warn "Apple Silicon detected — Linux and Windows builds will run under QEMU emulation."
  warn "Cold build: ~15-20 min each. Warm builds (cached target/) are much faster."
  warn "Do NOT wipe target/linux-x86_64 or target/x86_64-pc-windows-gnu between runs."
fi

build_linux
build_macos
build_windows
deploy_binaries

log "All builds complete! 🎉"
echo ""
echo "Download URLs (relative, served by web UI):"
echo "  Linux:   /bin/ifc2lbd-neo-linux"
echo "  macOS:   /bin/ifc2lbd-neo-macos"
echo "  Windows: /bin/ifc2lbd-neo-windows.exe"
