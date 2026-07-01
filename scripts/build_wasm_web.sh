#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR_32="$ROOT_DIR/web/wasm-prototype/src/wasm"
OUT_DIR_64="$ROOT_DIR/web/wasm-prototype/src/wasm64"

mkdir -p "$OUT_DIR_32" "$OUT_DIR_64"

cd "$ROOT_DIR"

if ! command -v rustup >/dev/null 2>&1; then
	echo "rustup is required for wasm nightly build." >&2
	exit 1
fi

NIGHTLY_CARGO="$(rustup which --toolchain nightly cargo)"
NIGHTLY_RUSTC="$(rustup which --toolchain nightly rustc)"

if [[ ! -x "$NIGHTLY_CARGO" || ! -x "$NIGHTLY_RUSTC" ]]; then
	echo "nightly cargo/rustc not found. Install with: rustup toolchain install nightly" >&2
	exit 1
fi

# ---------------------------------------------------------------------------
# Build + post-process a single WASM target.
#   $1 = target triple (e.g. wasm32-unknown-unknown)
#   $2 = output directory
#   $3 = max memory pages for the shared-memory patch
#        (wasm32: 65535 ≈ 4 GiB, wasm64: 262144 = 16 GiB)
# ---------------------------------------------------------------------------
build_target() {
	local target="$1"
	local outdir="$2"
	local max_pages="$3"

	echo "=== Building $target → $outdir ==="

	RUSTC="$NIGHTLY_RUSTC" "$NIGHTLY_CARGO" build \
		-Z build-std=std,panic_abort \
		--target "$target" \
		-p ifc2lbd-wasm \
		--release

	wasm-bindgen \
		--target web \
		--out-dir "$outdir" \
		"$ROOT_DIR/target/$target/release/ifc2lbd_wasm.wasm"

	local bg_wasm="$outdir/ifc2lbd_wasm_bg.wasm"

	# Ensure wasm-bindgen output preserves shared memory for threaded rayon runtime.
	# In some toolchain combinations the generated *_bg.wasm memory loses `shared`.
	if command -v wasm2wat >/dev/null 2>&1 && command -v wat2wasm >/dev/null 2>&1; then
		local tmp_wat
		tmp_wat="$(mktemp)"
		wasm2wat "$bg_wasm" -o "$tmp_wat" --enable-all
		python3 - "$tmp_wat" "$max_pages" <<'PY'
import re
import sys
from pathlib import Path

p = Path(sys.argv[1])
max_pages = sys.argv[2]
s = p.read_text()

# Match memory declarations for both wasm32 and wasm64:
#   (memory (;0;) 189 65535 shared)          — wasm32
#   (memory (;0;) i64 189 262144 shared)      — wasm64
m = re.search(r"\(memory \(;0;\) (i64 )?([0-9]+)(?: ([0-9]+))?(?: shared)?\)", s)
if not m:
    raise SystemExit("memory declaration not found in wasm2wat output")

addr_type = m.group(1) or ""
initial = m.group(2)
# Preserve the existing maximum if present, otherwise use initial
maximum = m.group(3) if m.group(3) else max_pages
# If the max from the linker is 0 or missing, use our max_pages
if not maximum or maximum == "0":
    maximum = max_pages

old = m.group(0)
new = f"(memory (;0;) {addr_type}{initial} {maximum} shared)"
p.write_text(s.replace(old, new, 1))
print(f"patched memory: {old} -> {new}")
PY
		wat2wasm "$tmp_wat" -o "$bg_wasm" --enable-all
		rm -f "$tmp_wat"
	fi

	# Patch rayon worker helpers to import from the correct relative path.
	local worker_helpers
	worker_helpers=$(find "$outdir/snippets" -name "workerHelpers.js" -type f 2>/dev/null | head -1)
	if [[ -n "$worker_helpers" && -f "$worker_helpers" ]]; then
		sed -i '' "s|await import('../../..')|await import('../../../ifc2lbd_wasm.js')|g" "$worker_helpers" 2>/dev/null ||
			sed -i "s|await import('../../..')|await import('../../../ifc2lbd_wasm.js')|g" "$worker_helpers"
		echo "Patched worker helpers: $worker_helpers"
	fi

	echo "✓ $target done → $outdir"
}

# ---------------------------------------------------------------------------
# Build both targets
# ---------------------------------------------------------------------------

# wasm32 (4 GiB max — fast, no bounds checks on 64-bit systems)
build_target "wasm32-unknown-unknown" "$OUT_DIR_32" "65535"

# wasm64 (16 GiB max — for large files that exceed the 4 GiB wasm32 cap)
build_target "wasm64-unknown-unknown" "$OUT_DIR_64" "262144"

echo ""
echo "WASM web artifacts written to:"
echo "  wasm32: $OUT_DIR_32"
echo "  wasm64: $OUT_DIR_64"
