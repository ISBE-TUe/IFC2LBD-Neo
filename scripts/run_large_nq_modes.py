#!/usr/bin/env python3

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
MODEL = ROOT / "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc"
ARTIFACTS = ROOT / "artifacts" / "paper-benchmarks"
BASE_URI = "https://benchmark.test/coord/"
CORES = {
    1: "0",
    2: "0-1",
    4: "0-3",
    8: "0-7",
    16: "0-15",
}
FAMILIES = [
    ("nq_ifcowl", ARTIFACTS / "large_scalability", []),
    ("nq_topology", ARTIFACTS / "large_scalability_topology", ["--topology"]),
    ("nq_full_topology", ARTIFACTS / "large_scalability_full_topology", ["--topology-full"]),
]


def main() -> None:
    for _, directory, _ in FAMILIES:
        directory.mkdir(parents=True, exist_ok=True)
    for _, directory, extra_args in FAMILIES:
        summary_path = directory / "summary.json"
        summary = json.loads(summary_path.read_text()) if summary_path.exists() else {}
        for core_count, core_mask in CORES.items():
            run_key = f"{core_count}core_nq"
            if run_key in summary and str(summary[run_key].get("returncode")) == "0":
                continue
            run_dir = directory / run_key
            run_dir.mkdir(parents=True, exist_ok=True)
            summary[run_key] = run_one(run_dir, core_count, core_mask, extra_args)
            summary_path.write_text(json.dumps(summary, indent=2) + "\n")


def run_one(run_dir: Path, core_count: int, core_mask: str, extra_args: list[str]) -> dict[str, object]:
    for path in run_dir.iterdir():
        if path.is_file():
            path.unlink()
    command = [
        "/usr/bin/time",
        "-v",
        "taskset",
        "-c",
        core_mask,
        str(BIN),
        str(MODEL),
        "--output-format",
        "nquads",
        "--output",
        str(run_dir / "out.nq"),
        "--base-uri",
        BASE_URI,
        "--ifcowl",
        "--quad-chunking",
        "cores",
        "--quad-chunk-core-count",
        str(core_count),
        "--quad-chunk-prefix",
        "large",
        *extra_args,
    ]
    (run_dir / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    write_df(run_dir / "disk_before.txt")
    with (run_dir / "stdout.txt").open("w") as stdout_file, (run_dir / "stderr.txt").open("w") as stderr_file:
        result = subprocess.run(command, cwd=ROOT, stdout=stdout_file, stderr=stderr_file, text=True)
    write_df(run_dir / "disk_after.txt")
    (run_dir / "returncode.txt").write_text(f"{result.returncode}\n")

    stderr = (run_dir / "stderr.txt").read_text()
    manifest_stats = collect_manifest_stats(run_dir)
    summary = {
        "returncode": result.returncode,
        "wall": extract(stderr, r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\):\s+([0-9:.]+)"),
        "user_seconds": extract(stderr, r"User time \(seconds\):\s+([0-9.]+)"),
        "sys_seconds": extract(stderr, r"System time \(seconds\):\s+([0-9.]+)"),
        "max_rss_kbytes": extract(stderr, r"Maximum resident set size \(kbytes\):\s+([0-9]+)"),
        "total_output_bytes": manifest_stats["total_bytes"],
        "chunk_files": manifest_stats["chunk_files"],
        "chunk_breakdown": manifest_stats["chunk_breakdown"],
        "command": " ".join(command),
        "core_count": core_count,
        "threads_label": thread_label(core_count),
    }
    before = parse_df(run_dir / "disk_before.txt")
    after = parse_df(run_dir / "disk_after.txt")
    if before is not None and after is not None:
        summary["avail_before_gb"] = before
        summary["avail_after_gb"] = after
        summary["delta_gb"] = before - after

    for nq_file in run_dir.glob("*.nq"):
        nq_file.unlink()
    return summary


def collect_manifest_stats(run_dir: Path) -> dict[str, object]:
    total_bytes = 0
    chunk_files = 0
    chunk_breakdown: dict[str, dict[str, int]] = {}
    for manifest in sorted(run_dir.glob("*.manifest.json")):
        data = json.loads(manifest.read_text())
        files = data.get("files", [])
        total_bytes += sum(item["bytes"] for item in files)
        chunk_files += len(files)
        chunk_breakdown[manifest.name] = {
            "files": len(files),
            "bytes": sum(item["bytes"] for item in files),
            "total_lines": int(data.get("total_lines", 0)),
            "total_triples_estimate": int(data.get("total_triples_estimate", 0)),
            "core_chunk_count": int(data.get("core_chunk_count", 0)),
        }
    return {
        "total_bytes": total_bytes,
        "chunk_files": chunk_files,
        "chunk_breakdown": chunk_breakdown,
    }


def write_df(path: Path) -> None:
    result = subprocess.run(["df", "-B1", str(ROOT)], cwd=ROOT, capture_output=True, text=True, check=True)
    path.write_text(result.stdout)


def parse_df(path: Path) -> float | None:
    lines = path.read_text().splitlines()
    if not lines:
        return None
    parts = lines[-1].split()
    if len(parts) < 4:
        return None
    return int(parts[3]) / 1024.0 / 1024.0 / 1024.0


def extract(text: str, pattern: str) -> str:
    match = re.search(pattern, text)
    if not match:
        raise RuntimeError(f"pattern not found: {pattern}")
    return match.group(1)


def thread_label(core_count: int) -> str:
    return f"{core_count} core / 2 threads" if core_count == 1 else f"{core_count} cores / {core_count * 2} threads"


if __name__ == "__main__":
    main()
