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
    ("ttl_ifcowl", "TTL + IfcOWL", ARTIFACTS / "large_scalability_ttl", []),
    ("ttl_topology", "TTL + IfcOWL + --topology", ARTIFACTS / "large_scalability_topology_ttl", ["--topology"]),
    (
        "ttl_full_topology",
        "TTL + IfcOWL + --topology-full",
        ARTIFACTS / "large_scalability_full_topology_ttl",
        ["--topology-full"],
    ),
]


def main() -> None:
    for _, _, directory, _ in FAMILIES:
        directory.mkdir(parents=True, exist_ok=True)
    for _, _, directory, extra_args in FAMILIES:
        summary_path = directory / "summary.json"
        summary = json.loads(summary_path.read_text()) if summary_path.exists() else {}
        for core_count, core_mask in CORES.items():
            run_key = f"{core_count}core_ttl"
            if run_key in summary and str(summary[run_key].get("returncode")) == "0":
                continue
            run_dir = directory / run_key
            run_dir.mkdir(parents=True, exist_ok=True)
            summary[run_key] = run_one(run_dir, core_count, core_mask, extra_args)
            summary_path.write_text(json.dumps(summary, indent=2) + "\n")


def run_one(run_dir: Path, core_count: int, core_mask: str, extra_args: list[str]) -> dict[str, object]:
    for name in ("out.ttl", "out_ifcowl.ttl", "stdout.txt", "stderr.txt", "disk_before.txt", "disk_after.txt"):
        path = run_dir / name
        if path.exists():
            path.unlink()

    command = [
        "/usr/bin/time",
        "-v",
        "taskset",
        "-c",
        core_mask,
        str(BIN),
        str(MODEL),
        "--output",
        str(run_dir / "out.ttl"),
        "--base-uri",
        BASE_URI,
        "--ifcowl",
        *extra_args,
    ]

    (run_dir / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    write_df(run_dir / "disk_before.txt")
    with (run_dir / "stdout.txt").open("w") as stdout_file, (run_dir / "stderr.txt").open("w") as stderr_file:
        result = subprocess.run(command, cwd=ROOT, stdout=stdout_file, stderr=stderr_file, text=True)
    write_df(run_dir / "disk_after.txt")
    (run_dir / "returncode.txt").write_text(f"{result.returncode}\n")

    stderr = (run_dir / "stderr.txt").read_text()
    out_ttl = run_dir / "out.ttl"
    out_ifcowl = run_dir / "out_ifcowl.ttl"
    lbd_bytes = out_ttl.stat().st_size if out_ttl.exists() else 0
    ifcowl_bytes = out_ifcowl.stat().st_size if out_ifcowl.exists() else 0
    summary = {
        "returncode": result.returncode,
        "wall": extract(stderr, r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\):\s+([0-9:.]+)"),
        "user_seconds": extract(stderr, r"User time \(seconds\):\s+([0-9.]+)"),
        "sys_seconds": extract(stderr, r"System time \(seconds\):\s+([0-9.]+)"),
        "max_rss_kbytes": extract(stderr, r"Maximum resident set size \(kbytes\):\s+([0-9]+)"),
        "lbd_ttl_bytes": lbd_bytes,
        "ifcowl_ttl_bytes": ifcowl_bytes,
        "total_output_bytes": lbd_bytes + ifcowl_bytes,
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

    # Retain only metrics and logs; the large TTL outputs are not needed after size extraction.
    for path in (out_ttl, out_ifcowl):
        if path.exists():
            path.unlink()

    return summary


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
