#!/usr/bin/env bash
# Build ifc2lbd-neo CLI binaries for Linux and macOS locally.
#
# Windows is NOT built locally — cross-compiling from Linux hits
# windows-sys import-library issues. Use the GitHub Actions workflow
# (build-cli.yml) for Windows binaries.
#
# Platform strategy:
#   Linux   → Docker (linux/amd64) — reproducible, no host toolchain needed
#   macOS   → Native (aarch64-apple-darwin) — Docker can't produce macOS binaries
#   Windows → GitHub Actions CI only (build-cli.yml)
#
# Outputs (repo root, gitignored):
#   ./ifc2lbd-neo-linux-x86_64
#   ./ifc2lbd-neo-macos
#
# For published binaries, see GitHub Releases:
#   https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest
#
# Requirements:
#   - Docker Desktop (with QEMU for linux/amd64 emulation on Apple Silicon)
#   - Rust toolchain installed locally (for macOS native build)

set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"

# ─── Helpers ─────────────────────────────────────────────────────────────────

log() { echo -e "\n\033[1;32m>>> $*\033[0m"; }
warn() { echo -e "\033[1;33m!! $*\033[0m"; }
die() {
	echo -e "\033[1;31m✗ $*\033[0m"
	exit 1
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

build_macos() {
	log "Building macOS aarch64 binary (native)..."

	cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo --target aarch64-apple-darwin

	cp "target/aarch64-apple-darwin/release/ifc2lbd-neo" "$REPO/ifc2lbd-neo-macos"
	chmod +x "$REPO/ifc2lbd-neo-macos"
	log "✓ macOS: $(du -h "$REPO/ifc2lbd-neo-macos" | cut -f1)"
	file "$REPO/ifc2lbd-neo-macos"
}

# ─── Main ────────────────────────────────────────────────────────────────────

command -v docker >/dev/null 2>&1 || die "Docker is required. Install Docker Desktop."
docker info >/dev/null 2>&1 || die "Docker is not running."
command -v cargo >/dev/null 2>&1 || die "Rust toolchain required for macOS native build."

if [[ "$(uname -m)" == "arm64" ]]; then
	warn "Apple Silicon — Linux build runs under QEMU emulation (~10 min cold, fast warm)."
fi

build_linux
build_macos

log "Local builds complete! 🎉"
log "Windows: use GitHub Actions (build-cli.yml) — push a tag or run manually."
echo ""
echo "Published binaries: https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest"
