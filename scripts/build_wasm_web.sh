#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/web/wasm-prototype/src/wasm"

mkdir -p "$OUT_DIR"

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

# opt-level=z is set in .cargo/config.toml for wasm32-unknown-unknown alongside the threading flags.
RUSTC="$NIGHTLY_RUSTC" "$NIGHTLY_CARGO" build \
  -Z build-std=std,panic_abort \
  --target wasm32-unknown-unknown \
  -p ifc2lbd-wasm \
  --release

wasm-bindgen \
  --target web \
  --out-dir "$OUT_DIR" \
  "$ROOT_DIR/target/wasm32-unknown-unknown/release/ifc2lbd_wasm.wasm"

BG_WASM="$OUT_DIR/ifc2lbd_wasm_bg.wasm"

# wasm-opt -Oz: second-pass size optimisation (typically saves another 10-15 %).
# Must run BEFORE the shared-memory patch (wasm-opt does not preserve the shared flag).
if command -v wasm-opt >/dev/null 2>&1; then
  echo "Running wasm-opt -Oz …"
  wasm-opt -Oz \
    --enable-bulk-memory \
    --enable-threads \
    --enable-simd \
    "$BG_WASM" -o "$BG_WASM"
fi

# Ensure wasm-bindgen output preserves shared memory for threaded rayon runtime.
# In some toolchain combinations the generated *_bg.wasm memory loses `shared`.
if command -v wasm2wat >/dev/null 2>&1 && command -v wat2wasm >/dev/null 2>&1; then
  TMP_WAT="$(mktemp)"
  wasm2wat "$BG_WASM" -o "$TMP_WAT" --enable-all
  python3 - "$TMP_WAT" <<'PY'
import re
import sys
from pathlib import Path

p = Path(sys.argv[1])
s = p.read_text()
m = re.search(r"\(memory \(;0;\) [0-9]+(?: [0-9]+)?(?: shared)?\)", s)
if not m:
    raise SystemExit("memory declaration not found in wasm2wat output")
old = m.group(0)
new = "(memory (;0;) 189 65535 shared)"
p.write_text(s.replace(old, new, 1))
print(f"patched memory: {old} -> {new}")
PY
  wat2wasm "$TMP_WAT" -o "$BG_WASM" --enable-all
  rm -f "$TMP_WAT"
fi

WORKER_HELPERS="$OUT_DIR/snippets/wasm-bindgen-rayon-38edf6e439f6d70d/src/workerHelpers.js"
if [[ -f "$WORKER_HELPERS" ]]; then
  # macOS sed requires an explicit backup extension with -i; use '' for in-place without backup
  sed -i '' "s|await import('../../..')|await import('../../../ifc2lbd_wasm.js')|g" "$WORKER_HELPERS" 2>/dev/null \
    || sed -i "s|await import('../../..')|await import('../../../ifc2lbd_wasm.js')|g" "$WORKER_HELPERS"
fi

echo "WASM web artifacts written to: $OUT_DIR"
