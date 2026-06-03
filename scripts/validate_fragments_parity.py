#!/usr/bin/env python3
"""
Compare the native fragments producer output with ThatOpen engine_fragment.

Usage:
    python3 scripts/validate_fragments_parity.py path/to/model.ifc

Notes:
    - The script clones engine_fragment into /tmp if absent.
    - It expects Node.js and npm/yarn to be available.
    - It compares both compressed bytes and decompressed FlatBuffer payloads.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ENGINE_DIR = Path("/tmp/engine_fragment")
NATIVE_BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
MODULES = [
    "neo-geometry-preprocess",
    "neo-geometry-producer",
    "neo-file-export",
]
MODULE_OPTS = [
    "neo-geometry-producer.format=fragments",
]


def run(cmd: list[str], cwd: Path | None = None) -> None:
    result = subprocess.run(cmd, cwd=cwd, text=True)
    if result.returncode != 0:
        raise SystemExit(result.returncode)


def ensure_engine_fragment() -> Path:
    if not ENGINE_DIR.exists():
        run(["git", "clone", "--depth", "1", "https://github.com/ThatOpen/engine_fragment.git", str(ENGINE_DIR)])
    return ENGINE_DIR


def ensure_engine_dependencies(engine_dir: Path) -> None:
    package_dir = engine_dir / "packages" / "fragments"
    if (package_dir / "node_modules").exists():
        return
    if shutil.which("npm"):
        run(["npm", "install"], cwd=engine_dir)
    elif shutil.which("yarn"):
        run(["yarn", "install"], cwd=engine_dir)
    else:
        raise RuntimeError("neither npm nor yarn is available")


def patch_engine_importer_runtime(engine_dir: Path) -> None:
    shim = engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "fragments-models-shim.ts"
    shim.write_text(
        """
export const ALIGNMENT_CATEGORY = "ThatOpenAlignment";
export const GRID_CATEGORY = "ThatOpenGrid";

export enum AlignmentCurveType {
  NONE = 0,
  LINES = 1,
  CLOTHOID = 2,
  ELLIPSE_ARC = 3,
  PARABOLA = 4,
}

export type AlignmentCurve = {
  points: Float32Array | number[];
  type: AlignmentCurveType;
};

export type AlignmentData = {
  absolute: AlignmentCurve[];
  horizontal: AlignmentCurve[];
  vertical: AlignmentCurve[];
};

export type GridAxisData = {
  tag: string;
  curve: number[];
};

export type GridData = {
  id: number;
  transform: number[];
  uAxes: GridAxisData[];
  vAxes: GridAxisData[];
  wAxes: GridAxisData[];
};
""".strip()
    )

    replacements = {
        engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "geometry" / "index.ts":
            ('from "../../../../FragmentsModels";', 'from "../fragments-models-shim";'),
        engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "geometry" / "ifc-file-reader.ts":
            ('from "../../../../FragmentsModels";', 'from "../fragments-models-shim";'),
        engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "geometry" / "grid-reader.ts":
            ('from "../../../../FragmentsModels";', 'from "../fragments-models-shim";'),
        engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "geometry" / "ifc" / "civil-reader.ts":
            ('from "../../../../../FragmentsModels";', 'from "../../fragments-models-shim";'),
        engine_dir / "packages" / "fragments" / "src" / "Importers" / "IfcImporter" / "src" / "properties" / "property-processor.ts":
            ('from "../../../../FragmentsModels";', 'from "../fragments-models-shim";'),
    }

    for path, (old, new) in replacements.items():
        text = path.read_text()
        if new in text:
            continue
        path.write_text(text.replace(old, new))


def build_native_release() -> None:
    run(["cargo", "build", "--release", "-p", "ifc2lbd-cli"], cwd=ROOT)


def native_fragments(ifc_path: Path, out_dir: Path) -> Path:
    cmd = [str(NATIVE_BIN), str(ifc_path), "-o", str(out_dir / "model.ttl")]
    for module in MODULES:
        cmd += ["--module", module]
    for opt in MODULE_OPTS:
        cmd += ["--module-opt", opt]
    run(cmd, cwd=ROOT)
    candidate = out_dir / "model.frag"
    if not candidate.exists():
        raise FileNotFoundError(candidate)
    return candidate


def oracle_fragments(ifc_path: Path, out_dir: Path) -> Path:
    engine_dir = ensure_engine_fragment()
    ensure_engine_dependencies(engine_dir)
    patch_engine_importer_runtime(engine_dir)
    script = f"""
import fs from "fs";
import path from "path";
import {{ IfcImporter }} from "{(engine_dir / 'packages/fragments/src/Importers/IfcImporter/index.ts').as_posix()}";

async function main() {{
  const serializer = new IfcImporter();
  serializer.wasm = {{ path: "{(engine_dir / 'node_modules/web-ifc/').as_posix()}/", absolute: true }};
  const input = fs.readFileSync("{ifc_path.as_posix()}");
  const output = await serializer.process({{ bytes: new Uint8Array(input), raw: false, id: "ifc2lbd-parity" }});
  fs.writeFileSync("{(out_dir / 'oracle.frag').as_posix()}", output);
}}

main().catch((error) => {{
  console.error(error);
  process.exit(1);
}});
"""
    script_path = out_dir / "oracle.ts"
    script_path.write_text(script)
    if shutil.which("npx") is None:
        raise RuntimeError("npx not found")
    run(
        [
            "npx",
            "-y",
            "node@20",
            str(engine_dir / "node_modules" / ".bin" / "tsx"),
            str(script_path),
        ],
        cwd=engine_dir,
    )
    return out_dir / "oracle.frag"


def compare(native_path: Path, oracle_path: Path) -> int:
    native = native_path.read_bytes()
    oracle = oracle_path.read_bytes()
    print(f"native compressed bytes: {len(native)}")
    print(f"oracle compressed bytes: {len(oracle)}")
    if native == oracle:
        print("compressed output matches byte-for-byte")
    else:
        print("compressed output differs")

    native_raw = zlib.decompress(native)
    oracle_raw = zlib.decompress(oracle)
    print(f"native raw bytes: {len(native_raw)}")
    print(f"oracle raw bytes: {len(oracle_raw)}")
    if native_raw == oracle_raw:
        print("raw flatbuffer payload matches byte-for-byte")
        return 0

    print("raw flatbuffer payload differs")
    return 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("ifc", type=Path)
    args = parser.parse_args()

    build_native_release()

    with tempfile.TemporaryDirectory() as tmp:
        out_dir = Path(tmp)
        native = native_fragments(args.ifc.resolve(), out_dir)
        oracle = oracle_fragments(args.ifc.resolve(), out_dir)
        return compare(native, oracle)


if __name__ == "__main__":
    sys.exit(main())
