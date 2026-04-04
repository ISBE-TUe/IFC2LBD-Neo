#!/usr/bin/env python3

import json
import math
import platform
import re
import shutil
import subprocess
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "artifacts" / "paper-benchmarks"
RUST_BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
COMPARE_BIN = ROOT / "target" / "release" / "compare-turtle"

JAVA_MAIN = "org.linkedbuildingdata.ifc2lbd.IFCtoLBDConverter_CLI"
JAVA_CP = (
    "/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/IFCtoLBD/"
    "IFCtoLBD_Python/jars/ifc-to-lbd-2.44.0.jar:"
    "/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/IFCtoLBD/IFCtoLBD_Python/jars/*"
)

DIGITALHUB = ROOT / "DigitalHub_FM-ARC_v2.ifc"
WOHN = ROOT / "Wohn-Geschaeftshaus.ifc"
LARGE = ROOT / "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc"

BASE_URIS = {
    "digitalhub": "https://benchmark.test/digitalhub/",
    "wohn": "https://benchmark.test/wohn/",
    "large": "https://benchmark.test/coord/",
}

RUST_COLOR = "#1f77b4"
JAVA_COLOR = "#ff7f0e"
MODE_COLORS = ["#1f77b4", "#2ca02c", "#d62728", "#9467bd"]


@dataclass
class RunResult:
    label: str
    model: str
    backend: str
    mode: str
    command: list[str]
    returncode: int
    wall_seconds: float | None
    user_seconds: float | None
    sys_seconds: float | None
    max_resident_bytes: int | None
    output_bytes: int
    output_breakdown: dict[str, int]
    lbd_triples: int | None
    ifcowl_triples: int | None
    notes: list[str]
    compare_summary: dict[str, str] | None = None


def main() -> None:
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    ensure_binaries()

    report: dict[str, object] = {
        "host": host_info(),
        "models": {
            "digitalhub": file_info(DIGITALHUB),
            "wohn": file_info(WOHN),
            "large": file_info(LARGE) if LARGE.exists() else None,
        },
    }

    digitalhub_compare = run_digitalhub_compare()
    digitalhub_modes = run_digitalhub_modes()
    wohn_bbox = run_wohn_bbox()
    large_case = run_large_case() if LARGE.exists() else None

    report["digitalhub_compare"] = {k: asdict(v) for k, v in digitalhub_compare.items()}
    report["digitalhub_modes"] = {k: asdict(v) for k, v in digitalhub_modes.items()}
    report["wohn_bbox"] = wohn_bbox
    report["large_case"] = asdict(large_case) if large_case else None

    (ARTIFACTS / "paper_benchmarks_report.json").write_text(
        json.dumps(report, indent=2) + "\n"
    )
    write_markdown_summary(report)
    write_plots(digitalhub_compare, digitalhub_modes, wohn_bbox, large_case)


def ensure_binaries() -> None:
    subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "ifc2lbd-cli",
            "--bin",
            "ifc2lbd-neo",
            "--bin",
            "compare-turtle",
        ],
        cwd=ROOT,
        check=True,
    )


def host_info() -> dict[str, str | int]:
    cpu = subprocess.run(
        ["sysctl", "-n", "hw.ncpu"], capture_output=True, text=True, check=True
    ).stdout.strip()
    brand = subprocess.run(
        ["sysctl", "-n", "machdep.cpu.brand_string"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    return {
        "platform": platform.platform(),
        "cpu_count": int(cpu),
        "cpu_brand": brand or "unknown",
    }


def file_info(path: Path | None) -> dict[str, object] | None:
    if path is None or not path.exists():
        return None
    return {
        "name": path.name,
        "bytes": path.stat().st_size,
        "megabytes": round(path.stat().st_size / (1024 * 1024), 2),
        "path": str(path),
    }


def run_digitalhub_compare() -> dict[str, RunResult]:
    rust = run_rust_turtle_ifcowl(
        label="Rust DigitalHub Turtle+IfcOWL",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        mode="turtle_ifcowl",
    )
    java = run_java_turtle_ifcowl(
        label="Java DigitalHub Level3+IfcOWL",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        mode="turtle_ifcowl_java_l3",
    )
    if rust.returncode == 0 and java.returncode == 0:
        compare = run_compare_if_possible(
            rust_out=ARTIFACTS / "digitalhub_rust_lbd.ttl",
            java_out=ARTIFACTS / "digitalhub_java_lbd.ttl",
            base_uri=BASE_URIS["digitalhub"],
        )
        rust.compare_summary = compare
        java.compare_summary = compare
    return {"rust": rust, "java": java}


def run_digitalhub_modes() -> dict[str, RunResult]:
    results: dict[str, RunResult] = {}
    results["ttl_ifcowl"] = run_rust_turtle_ifcowl(
        label="Rust DigitalHub Turtle+IfcOWL",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        mode="ttl_ifcowl_mode",
    )
    results["nquads_chunked"] = run_rust_nquads_chunked(
        label="Rust DigitalHub N-Quads chunked",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        prefix="digitalhub",
        mode="nquads_chunked_mode",
    )
    results["topology"] = run_rust_turtle_ifcowl(
        label="Rust DigitalHub Topology",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        mode="topology_mode",
        extra=["--topology"],
    )
    results["topology_full_bbox"] = run_rust_turtle_ifcowl(
        label="Rust DigitalHub Topology full + bbox",
        model_path=DIGITALHUB,
        base_uri=BASE_URIS["digitalhub"],
        mode="topology_full_bbox_mode",
        extra=["--topology-full", "--bbox"],
    )
    return results


def run_wohn_bbox() -> dict[str, object]:
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        out = tmpdir / "wohn.ttl"
        report_path = tmpdir / "bbox_report.json"
        cmd = [
            "/usr/bin/time",
            "-l",
            str(RUST_BIN),
            str(WOHN),
            "--output",
            str(out),
            "--base-uri",
            BASE_URIS["wohn"],
            "--bbox",
            "--bbox-report",
            str(report_path),
        ]
        completed = subprocess.run(
            cmd, cwd=ROOT, capture_output=True, text=True, check=False
        )
        timing = parse_time_output(completed.stderr)
        bbox_report = json.loads(report_path.read_text()) if report_path.exists() else {}
        result = {
            "command": cmd[2:],
            "returncode": completed.returncode,
            "wall_seconds": timing.get("wall_seconds"),
            "max_resident_bytes": timing.get("max_resident_bytes"),
            "lbd_bytes": out.stat().st_size if out.exists() else 0,
            "bbox_report": bbox_report,
        }
        (ARTIFACTS / "wohn_bbox_report.json").write_text(
            json.dumps(result, indent=2) + "\n"
        )
        return result


def run_large_case() -> RunResult:
    return run_rust_nquads_chunked(
        label="Rust Large model N-Quads chunked",
        model_path=LARGE,
        base_uri=BASE_URIS["large"],
        prefix="large",
        mode="large_nquads_chunked",
    )


def run_rust_turtle_ifcowl(
    label: str,
    model_path: Path,
    base_uri: str,
    mode: str,
    extra: list[str] | None = None,
) -> RunResult:
    extra = extra or []
    if model_path == DIGITALHUB:
        safe = "digitalhub_rust"
    else:
        safe = slugify(model_path.stem)
    lbd_out = ARTIFACTS / f"{safe}_{mode}_lbd.ttl"
    ifcowl_out = ARTIFACTS / f"{safe}_{mode}_lbd_ifcowl.ttl"
    for path in (lbd_out, ifcowl_out):
        if path.exists():
            path.unlink()
    cmd = [
        "/usr/bin/time",
        "-l",
        str(RUST_BIN),
        str(model_path),
        "--output",
        str(lbd_out),
        "--ifcowl",
        "--base-uri",
        base_uri,
        *extra,
    ]
    completed = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, check=False
    )
    timing = parse_time_output(completed.stderr)
    breakdown = {}
    if lbd_out.exists():
        breakdown["lbd_ttl"] = lbd_out.stat().st_size
    if ifcowl_out.exists():
        breakdown["ifcowl_ttl"] = ifcowl_out.stat().st_size
    return RunResult(
        label=label,
        model=model_path.name,
        backend="rust",
        mode=mode,
        command=cmd[2:],
        returncode=completed.returncode,
        wall_seconds=timing.get("wall_seconds"),
        user_seconds=timing.get("user_seconds"),
        sys_seconds=timing.get("sys_seconds"),
        max_resident_bytes=timing.get("max_resident_bytes"),
        output_bytes=sum(breakdown.values()),
        output_breakdown=breakdown,
        lbd_triples=count_turtle_triples(lbd_out),
        ifcowl_triples=None,
        notes=["ifcowl_triple_count_skipped_for_runtime"],
    )


def run_java_turtle_ifcowl(
    label: str,
    model_path: Path,
    base_uri: str,
    mode: str,
) -> RunResult:
    lbd_out = ARTIFACTS / "digitalhub_java_lbd.ttl"
    ifcowl_out = ARTIFACTS / "digitalhub_java_lbd_ifcowl.ttl"
    for path in ARTIFACTS.glob("digitalhub_java_lbd*.ttl"):
        path.unlink()
    cmd = [
        "/usr/bin/time",
        "-l",
        "java",
        "-Xms256m",
        "-Xmx4G",
        "-cp",
        JAVA_CP,
        JAVA_MAIN,
        str(model_path),
        "--url",
        base_uri,
        "--level",
        "3",
        "--target_file",
        str(lbd_out),
        "--hasBuildingElements",
        "--hasBuildingElementProperties",
        "--hasUnits",
        "--hasGeolocation",
        "--ifcOWL",
    ]
    completed = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, check=False
    )
    timing = parse_time_output(completed.stderr)
    java_outputs = sorted(ARTIFACTS.glob("digitalhub_java_lbd*.ttl"))
    breakdown = {path.name: path.stat().st_size for path in java_outputs}
    notes = []
    if "RDFWriter" in completed.stderr or "ERROR" in completed.stderr:
        notes.append("java_stderr_contains_writer_errors")
    return RunResult(
        label=label,
        model=model_path.name,
        backend="java",
        mode=mode,
        command=cmd[2:],
        returncode=completed.returncode,
        wall_seconds=timing.get("wall_seconds"),
        user_seconds=timing.get("user_seconds"),
        sys_seconds=timing.get("sys_seconds"),
        max_resident_bytes=timing.get("max_resident_bytes"),
        output_bytes=sum(breakdown.values()),
        output_breakdown=breakdown,
        lbd_triples=count_turtle_triples(lbd_out),
        ifcowl_triples=None,
        notes=notes,
    )


def run_rust_nquads_chunked(
    label: str,
    model_path: Path,
    base_uri: str,
    prefix: str,
    mode: str,
) -> RunResult:
    workdir = ARTIFACTS / f"{slugify(model_path.stem)}_{mode}"
    if workdir.exists():
        shutil.rmtree(workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    output_file = workdir / "out.nq"
    cmd = [
        "/usr/bin/time",
        "-l",
        str(RUST_BIN),
        str(model_path),
        "--output-format",
        "nquads",
        "--output",
        str(output_file),
        "--base-uri",
        base_uri,
        "--ifcowl",
        "--quad-chunking",
        "cores",
        "--quad-chunk-prefix",
        prefix,
    ]
    completed = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, check=False
    )
    timing = parse_time_output(completed.stderr)
    breakdown = classify_chunked_outputs(workdir, prefix)
    return RunResult(
        label=label,
        model=model_path.name,
        backend="rust",
        mode=mode,
        command=cmd[2:],
        returncode=completed.returncode,
        wall_seconds=timing.get("wall_seconds"),
        user_seconds=timing.get("user_seconds"),
        sys_seconds=timing.get("sys_seconds"),
        max_resident_bytes=timing.get("max_resident_bytes"),
        output_bytes=sum(breakdown.values()),
        output_breakdown=breakdown,
        lbd_triples=None,
        ifcowl_triples=None,
        notes=[],
    )


def classify_chunked_outputs(workdir: Path, prefix: str) -> dict[str, int]:
    sizes = {
        "lbd_nq": 0,
        "ifcowl_nq": 0,
        "topology_nq": 0,
        "manifest_json": 0,
    }
    for path in workdir.iterdir():
        if not path.is_file():
            continue
        name = path.name
        size = path.stat().st_size
        if name.endswith(".json"):
            sizes["manifest_json"] += size
        elif f"{prefix}-ifcowl" in name:
            sizes["ifcowl_nq"] += size
        elif f"{prefix}-topology" in name:
            sizes["topology_nq"] += size
        elif f"{prefix}-lbd" in name:
            sizes["lbd_nq"] += size
        else:
            sizes.setdefault("other", 0)
            sizes["other"] += size
    return {k: v for k, v in sizes.items() if v > 0}


def run_compare_if_possible(rust_out: Path, java_out: Path, base_uri: str) -> dict[str, str]:
    if not COMPARE_BIN.exists() or not rust_out.exists() or not java_out.exists():
        return {}
    completed = subprocess.run(
        [
            str(COMPARE_BIN),
            str(java_out),
            str(rust_out),
            "--left-base",
            base_uri,
            "--right-base",
            base_uri,
            "--normalize-lbd-opm",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    summary = {}
    for line in completed.stdout.splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            if key in {"result", "total", "left_only", "right_only", "identical"}:
                summary[key] = value.strip()
    return summary


def count_turtle_triples(path: Path) -> int | None:
    if not path.exists():
        return None
    try:
        from rdflib import ConjunctiveGraph

        graph = ConjunctiveGraph()
        graph.parse(str(path), format="turtle")
        return len(graph)
    except Exception:
        return None


def parse_time_output(stderr: str) -> dict[str, float | int | None]:
    real_match = re.search(
        r"^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys$",
        stderr,
        re.MULTILINE,
    )
    rss_match = re.search(r"^\s*([0-9]+)\s+maximum resident set size$", stderr, re.MULTILINE)
    result: dict[str, float | int | None] = {
        "wall_seconds": None,
        "user_seconds": None,
        "sys_seconds": None,
        "max_resident_bytes": int(rss_match.group(1)) if rss_match else None,
    }
    if real_match:
        result["wall_seconds"] = float(real_match.group(1))
        result["user_seconds"] = float(real_match.group(2))
        result["sys_seconds"] = float(real_match.group(3))
    return result


def write_markdown_summary(report: dict[str, object]) -> None:
    lines = [
        "# Paper Benchmark Summary",
        "",
        f"- Platform: `{report['host']['platform']}`",
        f"- CPU count: `{report['host']['cpu_count']}`",
        f"- CPU brand: `{report['host']['cpu_brand']}`",
        "",
        "## DigitalHub Comparison",
        "",
        "| Backend | Wall (s) | Peak RSS (MB) | Output (MB) | LBD triples | IfcOWL triples |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for key in ("rust", "java"):
        result = report["digitalhub_compare"][key]
        lines.append(
            "| {label} | {wall:.2f} | {rss:.1f} | {out_mb:.1f} | {lbd} | {ifcowl} |".format(
                label=result["label"],
                wall=result["wall_seconds"] or math.nan,
                rss=((result["max_resident_bytes"] or 0) / (1024 * 1024)),
                out_mb=result["output_bytes"] / (1024 * 1024),
                lbd=result["lbd_triples"] if result["lbd_triples"] is not None else "n/a",
                ifcowl=(
                    result["ifcowl_triples"]
                    if result["ifcowl_triples"] is not None
                    else "n/a"
                ),
            )
        )
    lines.extend(
        [
            "",
            "## DigitalHub Rust Modes",
            "",
            "| Mode | Wall (s) | Peak RSS (MB) | Output (MB) |",
            "| --- | ---: | ---: | ---: |",
        ]
    )
    for key, result in report["digitalhub_modes"].items():
        lines.append(
            "| {mode} | {wall:.2f} | {rss:.1f} | {out_mb:.1f} |".format(
                mode=key,
                wall=result["wall_seconds"] or math.nan,
                rss=((result["max_resident_bytes"] or 0) / (1024 * 1024)),
                out_mb=result["output_bytes"] / (1024 * 1024),
            )
        )
    bbox = report["wohn_bbox"]["bbox_report"]
    lines.extend(
        [
            "",
            "## Wohn Bounding Boxes",
            "",
            f"- Elements requested: `{bbox['elements_requested']}`",
            f"- Elements with mesh: `{bbox['elements_with_mesh']}`",
            f"- Exact escalation count: `{bbox['escalated_exact_count']}`",
            f"- Rotated bbox count: `{bbox['rotated_bbox_count']}`",
            f"- Fast bbox inflation > 1.5: `{bbox['count_fast_over_1_5']}`",
            f"- Fast bbox inflation > 2.0: `{bbox['count_fast_over_2_0']}`",
            "",
        ]
    )
    if report["large_case"]:
        large = report["large_case"]
        lines.extend(
            [
                "## Large Model",
                "",
                f"- Model: `{large['model']}`",
                f"- Wall time: `{large['wall_seconds']:.2f} s`",
                f"- Peak RSS: `{(large['max_resident_bytes'] or 0)/(1024*1024):.1f} MB`",
                f"- Output total: `{large['output_bytes']/(1024*1024):.1f} MB`",
                "",
            ]
        )
    (ARTIFACTS / "paper_benchmarks_report.md").write_text("\n".join(lines))


def write_plots(
    digitalhub_compare: dict[str, RunResult],
    digitalhub_modes: dict[str, RunResult],
    wohn_bbox: dict[str, object],
    large_case: RunResult | None,
) -> None:
    plot_digitalhub_compare(digitalhub_compare)
    plot_digitalhub_modes(digitalhub_modes)
    plot_wohn_bbox(wohn_bbox)
    if large_case:
        plot_large_case(large_case)


def plot_digitalhub_compare(results: dict[str, RunResult]) -> None:
    rust = results["rust"]
    java = results["java"]
    fig, axes = plt.subplots(1, 3, figsize=(13, 4.2))
    labels = ["Rust", "Java"]
    colors = [RUST_COLOR, JAVA_COLOR]

    wall = [rust.wall_seconds or 0.0, java.wall_seconds or 0.0]
    rss = [
        ((rust.max_resident_bytes or 0) / (1024 * 1024)),
        ((java.max_resident_bytes or 0) / (1024 * 1024)),
    ]
    out_mb = [rust.output_bytes / (1024 * 1024), java.output_bytes / (1024 * 1024)]

    for ax, values, title, ylabel in [
        (axes[0], wall, "Wall Time", "seconds"),
        (axes[1], rss, "Peak RSS", "MB"),
        (axes[2], out_mb, "Total Output", "MB"),
    ]:
        bars = ax.bar(labels, values, color=colors, alpha=0.9)
        ax.set_title(title)
        ax.set_ylabel(ylabel)
        ax.grid(axis="y", alpha=0.3)
        for bar, value in zip(bars, values):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                value,
                f"{value:.1f}",
                ha="center",
                va="bottom",
                fontsize=9,
            )
    fig.suptitle("DigitalHub_FM-ARC_v2.ifc: Rust vs Java baseline")
    fig.tight_layout()
    fig.savefig(ARTIFACTS / "digitalhub_compare.png", dpi=180, bbox_inches="tight")
    plt.close(fig)


def plot_digitalhub_modes(results: dict[str, RunResult]) -> None:
    keys = list(results.keys())
    labels = ["TTL+IfcOWL", "NQ chunked", "Topology", "Full topo+bbox"]
    wall = [results[key].wall_seconds or 0.0 for key in keys]
    rss = [((results[key].max_resident_bytes or 0) / (1024 * 1024)) for key in keys]
    out_mb = [results[key].output_bytes / (1024 * 1024) for key in keys]

    fig, axes = plt.subplots(1, 3, figsize=(14, 4.5))
    for ax, values, title, ylabel in [
        (axes[0], wall, "Wall Time by Mode", "seconds"),
        (axes[1], rss, "Peak RSS by Mode", "MB"),
        (axes[2], out_mb, "Output Size by Mode", "MB"),
    ]:
        bars = ax.bar(labels, values, color=MODE_COLORS, alpha=0.9)
        ax.set_title(title)
        ax.set_ylabel(ylabel)
        ax.tick_params(axis="x", rotation=18)
        ax.grid(axis="y", alpha=0.3)
        for bar, value in zip(bars, values):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                value,
                f"{value:.1f}",
                ha="center",
                va="bottom",
                fontsize=8,
            )
    fig.suptitle("DigitalHub_FM-ARC_v2.ifc: IFC2LBD-Neo mode comparison")
    fig.tight_layout()
    fig.savefig(ARTIFACTS / "digitalhub_modes.png", dpi=180, bbox_inches="tight")
    plt.close(fig)


def plot_wohn_bbox(wohn_bbox: dict[str, object]) -> None:
    bbox = wohn_bbox["bbox_report"]
    labels = [
        "Escalated exact",
        "Rotated bbox",
        "Fast infl. > 1.5",
        "Fast infl. > 2.0",
    ]
    values = [
        bbox["escalated_exact_count"],
        bbox["rotated_bbox_count"],
        bbox["count_fast_over_1_5"],
        bbox["count_fast_over_2_0"],
    ]
    fig, ax = plt.subplots(figsize=(8.5, 4.5))
    bars = ax.bar(labels, values, color=["#d62728", "#9467bd", "#2ca02c", "#8c564b"])
    ax.set_title("Wohn-Geschaeftshaus.ifc: bbox escalation signals")
    ax.set_ylabel("element count")
    ax.grid(axis="y", alpha=0.3)
    for bar, value in zip(bars, values):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            value,
            str(value),
            ha="center",
            va="bottom",
            fontsize=9,
        )
    text = (
        f"avg fast inflation = {bbox['avg_inflation_fast']:.3f}\n"
        f"max fast inflation = {bbox['max_inflation_fast']:.3f}\n"
        f"avg final inflation = {bbox['avg_inflation_final']:.3f}\n"
        f"max final inflation = {bbox['max_inflation_final']:.3f}"
    )
    ax.text(
        0.98,
        0.96,
        text,
        transform=ax.transAxes,
        ha="right",
        va="top",
        fontsize=9,
        bbox=dict(boxstyle="round,pad=0.35", fc="white", ec="#cccccc"),
    )
    fig.tight_layout()
    fig.savefig(ARTIFACTS / "wohn_bbox_quality.png", dpi=180, bbox_inches="tight")
    plt.close(fig)


def plot_large_case(large_case: RunResult) -> None:
    labels = ["Wall time (s)", "Peak RSS (MB)", "Output (MB)"]
    values = [
        large_case.wall_seconds or 0.0,
        ((large_case.max_resident_bytes or 0) / (1024 * 1024)),
        large_case.output_bytes / (1024 * 1024),
    ]
    fig, ax = plt.subplots(figsize=(8.5, 4.2))
    bars = ax.bar(labels, values, color=["#1f77b4", "#ff7f0e", "#2ca02c"], alpha=0.9)
    ax.set_title(f"Large model scalability: {large_case.model}")
    ax.grid(axis="y", alpha=0.3)
    for bar, value in zip(bars, values):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            value,
            f"{value:.1f}",
            ha="center",
            va="bottom",
            fontsize=9,
        )
    fig.tight_layout()
    fig.savefig(ARTIFACTS / "large_model_scalability.png", dpi=180, bbox_inches="tight")
    plt.close(fig)


def slugify(value: str) -> str:
    value = value.lower()
    value = re.sub(r"[^a-z0-9]+", "_", value)
    return value.strip("_")


if __name__ == "__main__":
    main()
