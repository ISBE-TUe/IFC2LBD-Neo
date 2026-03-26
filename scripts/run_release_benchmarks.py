#!/usr/bin/env python3

import json
import platform
import re
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "artifacts" / "benchmarks"
BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
BASE_URI = "https://example.test/base/"


@dataclass
class BenchmarkResult:
    fixture: str
    fixture_bytes: int
    wall_seconds: float
    user_seconds: float
    sys_seconds: float
    max_resident_bytes: int | None
    lbd_bytes: int
    ifcowl_bytes: int


def main() -> None:
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    build_release_binary()

    fixtures = [
        ROOT / "Duplex.ifc",
        ROOT / "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc",
    ]
    fixtures = [path for path in fixtures if path.exists()]

    results = [run_fixture(path) for path in fixtures]

    report = {
        "host": {
            "platform": platform.platform(),
            "cpu_count": cpu_count(),
        },
        "results": [asdict(result) for result in results],
    }

    (ARTIFACTS / "release_benchmark_report.json").write_text(
        json.dumps(report, indent=2) + "\n"
    )
    (ARTIFACTS / "release_benchmark_report.md").write_text(render_markdown(report))


def build_release_binary() -> None:
    subprocess.run(
        ["cargo", "build", "--release", "-p", "ifc2lbd-cli", "--bin", "ifc2lbd-neo"],
        cwd=ROOT,
        check=True,
    )


def run_fixture(path: Path) -> BenchmarkResult:
    slug = slugify(path.stem)
    lbd_out = ARTIFACTS / f"{slug}_lbd.ttl"
    ifcowl_out = ARTIFACTS / f"{slug}_lbd_ifcowl.ttl"

    for out in (lbd_out, ifcowl_out):
        if out.exists():
            out.unlink()

    cmd = [
        "/usr/bin/time",
        "-l",
        str(BIN),
        str(path),
        "--output",
        str(lbd_out),
        "--ifcowl",
        "--base-uri",
        BASE_URI,
        "--topology",
    ]

    completed = subprocess.run(
        cmd,
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )

    timing = parse_time_output(completed.stderr)
    return BenchmarkResult(
        fixture=path.name,
        fixture_bytes=path.stat().st_size,
        wall_seconds=timing["wall_seconds"],
        user_seconds=timing["user_seconds"],
        sys_seconds=timing["sys_seconds"],
        max_resident_bytes=timing["max_resident_bytes"],
        lbd_bytes=lbd_out.stat().st_size if lbd_out.exists() else 0,
        ifcowl_bytes=ifcowl_out.stat().st_size if ifcowl_out.exists() else 0,
    )


def parse_time_output(stderr: str) -> dict[str, float | int | None]:
    real_match = re.search(
        r"^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys$",
        stderr,
        re.MULTILINE,
    )
    if not real_match:
        raise RuntimeError(f"could not parse /usr/bin/time output:\n{stderr}")

    rss_match = re.search(r"^\s*([0-9]+)\s+maximum resident set size$", stderr, re.MULTILINE)
    max_rss = int(rss_match.group(1)) if rss_match else None

    return {
        "wall_seconds": float(real_match.group(1)),
        "user_seconds": float(real_match.group(2)),
        "sys_seconds": float(real_match.group(3)),
        "max_resident_bytes": max_rss,
    }


def render_markdown(report: dict) -> str:
    lines = [
        "# Release Benchmark Report",
        "",
        f"- Platform: `{report['host']['platform']}`",
        f"- CPU count seen by runner: `{report['host']['cpu_count']}`",
        "",
        "| Fixture | Size (MB) | Wall (s) | User (s) | Sys (s) | Max RSS (MB) | LBD out (MB) | IfcOWL out (MB) |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for result in report["results"]:
        lines.append(
            "| {fixture} | {fixture_mb:.1f} | {wall:.2f} | {user:.2f} | {sys:.2f} | {rss:.1f} | {lbd:.1f} | {ifcowl:.1f} |".format(
                fixture=result["fixture"],
                fixture_mb=result["fixture_bytes"] / (1024 * 1024),
                wall=result["wall_seconds"],
                user=result["user_seconds"],
                sys=result["sys_seconds"],
                rss=(result["max_resident_bytes"] or 0) / (1024 * 1024),
                lbd=result["lbd_bytes"] / (1024 * 1024),
                ifcowl=result["ifcowl_bytes"] / (1024 * 1024),
            )
        )
    lines.append("")
    return "\n".join(lines)


def cpu_count() -> int:
    if platform.system() == "Darwin":
        output = subprocess.run(
            ["sysctl", "-n", "hw.ncpu"],
            capture_output=True,
            text=True,
            check=True,
        )
        return int(output.stdout.strip())
    return int(subprocess.run(["getconf", "_NPROCESSORS_ONLN"], capture_output=True, text=True, check=True).stdout.strip())


def slugify(value: str) -> str:
    value = value.lower()
    value = re.sub(r"[^a-z0-9]+", "_", value)
    return value.strip("_")


if __name__ == "__main__":
    main()
