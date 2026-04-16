#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/web/wasm-prototype/src/wasm"

mkdir -p "$OUT_DIR"

cd "$ROOT_DIR"

cargo +nightly build \
  -Z build-std=std,panic_abort \
  --target wasm32-unknown-unknown \
  -p ifc2lbd-wasm \
  --release

wasm-bindgen \
  --target web \
  --out-dir "$OUT_DIR" \
  "$ROOT_DIR/target/wasm32-unknown-unknown/release/ifc2lbd_wasm.wasm"

WORKER_HELPERS="$OUT_DIR/snippets/wasm-bindgen-rayon-38edf6e439f6d70d/src/workerHelpers.js"
if [[ -f "$WORKER_HELPERS" ]]; then
  sed -i "s|await import('../../..')|await import('../../../ifc2lbd_wasm.js')|g" "$WORKER_HELPERS"
fi

echo "WASM web artifacts written to: $OUT_DIR"
