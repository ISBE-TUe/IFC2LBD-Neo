#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import math
import platform
import re
import shutil
import statistics
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


RUST_RED = "#8D6262"
JAVA_GREY = "#7A7D81"
MODE_GREYS = ["#6C7075", "#A1A4A8"]
MODE_REDS = ["#8D6262", "#B58F8F"]


@dataclass
class RunObservation:
    repeat_index: int
    returncode: int
    wall_seconds: float | None
    user_seconds: float | None
    sys_seconds: float | None
    max_resident_bytes: int | None
    primary_output_bytes: int
    output_breakdown: dict[str, int]
    stderr_tail: str
    run_dir: str


@dataclass
class AggregateResult:
    key: str
    label: str
    backend: str
    command: list[str]
    repeats: int
    successful_runs: int
    representative_run_dir: str | None
    wall_mean: float | None
    wall_std: float | None
    rss_mean_mb: float | None
    rss_std_mb: float | None
    output_mean_mb: float | None
    output_std_mb: float | None
    lbd_triples: int | None
    ifcowl_triples: int | None
    notes: list[str]
    observations: list[RunObservation]


def main() -> None:
    args = parse_args()
    out_dir = args.out_dir.resolve()
    if args.clean and out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    if args.build:
        build_release_binaries(args.root)

    report = {
        "host": host_info(),
        "model": {
            "name": args.ifc.name,
            "bytes": args.ifc.stat().st_size,
            "megabytes": round(args.ifc.stat().st_size / (1024 * 1024), 2),
            "path": str(args.ifc),
        },
        "repeats": args.repeats,
        "configs": {},
        "lbd_compare": {},
        "ifcowl_compare": {},
    }

    configs = digitalhub_configs(args, out_dir)
    aggregates: dict[str, AggregateResult] = {}
    for cfg in configs:
        aggregates[cfg["key"]] = run_repeated_config(cfg, args.repeats, args.root)

    report["configs"] = {key: asdict(value) for key, value in aggregates.items()}

    rust_baseline = aggregates["rust_ttl_ifcowl"]
    java_baseline = aggregates["java_ttl_ifcowl"]

    lbd_compare = run_compare_if_possible(
        compare_bin=args.compare_bin,
        left=representative_path(java_baseline, "out.ttl"),
        right=representative_path(rust_baseline, "out.ttl"),
        left_base=args.base_uri,
        right_base=args.base_uri,
        extra=["--normalize-lbd-opm"],
    )
    report["lbd_compare"] = lbd_compare

    if args.compare_ifcowl:
        ifcowl_compare = run_compare_if_possible(
            compare_bin=args.compare_bin,
            left=representative_path(java_baseline, "out_ifcowl.ttl"),
            right=representative_path(rust_baseline, "out_ifcowl.ttl"),
            left_base=args.base_uri,
            right_base=args.base_uri,
            extra=["--normalize-ifcowl-scalars", "--sample-limit", "10"],
        )
        report["ifcowl_compare"] = ifcowl_compare

    (out_dir / "digitalhub_report.json").write_text(json.dumps(report, indent=2) + "\n")
    (out_dir / "digitalhub_report.md").write_text(render_markdown(report))
    write_plots(out_dir, aggregates)


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(
        description="Repeated DigitalHub benchmark for the EG-ICE paper"
    )
    parser.add_argument("--root", type=Path, default=root)
    parser.add_argument("--ifc", type=Path, default=root / "DigitalHub_FM-ARC_v2.ifc")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=root / "artifacts" / "paper-digitalhub",
    )
    parser.add_argument("--base-uri", default="https://benchmark.test/digitalhub/")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--clean", action="store_true")
    parser.add_argument("--build", action="store_true")
    parser.add_argument(
        "--rust-bin",
        type=Path,
        default=root / "target" / "release" / "ifc2lbd-neo",
    )
    parser.add_argument(
        "--compare-bin",
        type=Path,
        default=root / "target" / "release" / "compare-turtle",
    )
    parser.add_argument(
        "--java-main",
        default="org.linkedbuildingdata.ifc2lbd.IFCtoLBDConverter_CLI",
    )
    parser.add_argument(
        "--java-cp",
        default=(
            "/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/IFCtoLBD/"
            "IFCtoLBD_Python/jars/ifc-to-lbd-2.44.0.jar:"
            "/Users/lukas.kirner/Projects/IFC2LBD-Rewrite/IFCtoLBD/IFCtoLBD_Python/jars/*"
        ),
    )
    parser.add_argument("--java-xms", default="256m")
    parser.add_argument("--java-xmx", default="16G")
    parser.add_argument("--compare-ifcowl", action="store_true")
    args = parser.parse_args()
    args.root = args.root.resolve()
    args.ifc = args.ifc.resolve()
    args.out_dir = args.out_dir.resolve()
    args.rust_bin = args.rust_bin.resolve()
    args.compare_bin = args.compare_bin.resolve()
    return args


def build_release_binaries(root: Path) -> None:
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
        cwd=root,
        check=True,
    )


def host_info() -> dict[str, str | int]:
    cpu_count = 0
    try:
        cpu_count = int(
            subprocess.run(
                ["getconf", "_NPROCESSORS_ONLN"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
        )
    except Exception:
        cpu_count = 0
    return {
        "platform": platform.platform(),
        "cpu_count": cpu_count,
    }


def digitalhub_configs(args: argparse.Namespace, out_dir: Path) -> list[dict[str, object]]:
    return [
        {
            "key": "java_ttl_ifcowl",
            "label": "Java LBD + IfcOWL (TTL)",
            "backend": "java",
            "subdir": out_dir / "java_ttl_ifcowl",
            "command_builder": lambda run_dir: [
                "java",
                f"-Xms{args.java_xms}",
                f"-Xmx{args.java_xmx}",
                "-cp",
                args.java_cp,
                args.java_main,
                str(args.ifc),
                "--url",
                args.base_uri,
                "--level",
                "3",
                "--target_file",
                str(run_dir / "out.ttl"),
                "--hasBuildingElements",
                "--hasBuildingElementProperties",
                "--hasUnits",
                "--hasGeolocation",
                "--ifcOWL",
            ],
        },
        {
            "key": "rust_ttl_ifcowl",
            "label": "Rust LBD + IfcOWL (TTL)",
            "backend": "rust",
            "subdir": out_dir / "rust_ttl_ifcowl",
            "command_builder": lambda run_dir: [
                str(args.rust_bin),
                str(args.ifc),
                "--output",
                str(run_dir / "out.ttl"),
                "--ifcowl",
                "--base-uri",
                args.base_uri,
            ],
        },
        {
            "key": "rust_nquads_chunked",
            "label": "Rust LBD + IfcOWL (N-Quads chunked)",
            "backend": "rust",
            "subdir": out_dir / "rust_nquads_chunked",
            "command_builder": lambda run_dir: [
                str(args.rust_bin),
                str(args.ifc),
                "--output-format",
                "nquads",
                "--output",
                str(run_dir / "out.nq"),
                "--base-uri",
                args.base_uri,
                "--ifcowl",
                "--quad-chunking",
                "cores",
                "--quad-chunk-prefix",
                "digitalhub",
            ],
        },
        {
            "key": "rust_topology",
            "label": "Rust LBD + IfcOWL + topology",
            "backend": "rust",
            "subdir": out_dir / "rust_topology",
            "command_builder": lambda run_dir: [
                str(args.rust_bin),
                str(args.ifc),
                "--output",
                str(run_dir / "out.ttl"),
                "--ifcowl",
                "--base-uri",
                args.base_uri,
                "--topology",
            ],
        },
        {
            "key": "rust_topology_full_bbox",
            "label": "Rust LBD + IfcOWL + full topology + bboxes",
            "backend": "rust",
            "subdir": out_dir / "rust_topology_full_bbox",
            "command_builder": lambda run_dir: [
                str(args.rust_bin),
                str(args.ifc),
                "--output",
                str(run_dir / "out.ttl"),
                "--ifcowl",
                "--base-uri",
                args.base_uri,
                "--topology-full",
                "--bbox",
            ],
        },
    ]


def run_repeated_config(cfg: dict[str, object], repeats: int, cwd: Path) -> AggregateResult:
    key = str(cfg["key"])
    label = str(cfg["label"])
    backend = str(cfg["backend"])
    subdir = Path(cfg["subdir"])
    if subdir.exists():
        shutil.rmtree(subdir)
    subdir.mkdir(parents=True, exist_ok=True)

    observations: list[RunObservation] = []
    representative_run_dir: Path | None = None
    representative_lbd: Path | None = None
    representative_ifcowl: Path | None = None
    command_example: list[str] = []

    for index in range(1, repeats + 1):
        run_dir = subdir / f"run_{index:02d}"
        run_dir.mkdir(parents=True, exist_ok=True)
        base_cmd = cfg["command_builder"](run_dir)
        command_example = base_cmd
        cmd = ["/usr/bin/time", "-v", *base_cmd]
        completed = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
        timing = parse_time_output(completed.stderr)
        output_breakdown = classify_outputs(run_dir)
        primary_output_bytes = sum(
            value
            for name, value in output_breakdown.items()
            if name in {"lbd_ttl", "ifcowl_ttl", "lbd_nq", "ifcowl_nq", "topology_nq"}
        )
        observations.append(
            RunObservation(
                repeat_index=index,
                returncode=completed.returncode,
                wall_seconds=timing.get("wall_seconds"),
                user_seconds=timing.get("user_seconds"),
                sys_seconds=timing.get("sys_seconds"),
                max_resident_bytes=timing.get("max_resident_bytes"),
                primary_output_bytes=primary_output_bytes,
                output_breakdown=output_breakdown,
                stderr_tail=completed.stderr[-3000:],
                run_dir=str(run_dir),
            )
        )
        if completed.returncode == 0 and representative_run_dir is None:
            representative_run_dir = run_dir
            representative_lbd = run_dir / "out.ttl"
            representative_ifcowl = run_dir / "out_ifcowl.ttl"

    success = [obs for obs in observations if obs.returncode == 0]
    notes = []
    if not success:
        notes.append("no_successful_runs")
    if any(obs.returncode != 0 for obs in observations):
        notes.append("contains_nonzero_runs")

    lbd_triples = count_turtle_triples(representative_lbd) if representative_lbd and representative_lbd.exists() else None
    ifcowl_triples = None

    return AggregateResult(
        key=key,
        label=label,
        backend=backend,
        command=command_example,
        repeats=repeats,
        successful_runs=len(success),
        representative_run_dir=str(representative_run_dir) if representative_run_dir else None,
        wall_mean=mean_or_none([obs.wall_seconds for obs in success]),
        wall_std=std_or_zero([obs.wall_seconds for obs in success]),
        rss_mean_mb=mean_or_none(
            [bytes_to_mb(obs.max_resident_bytes) for obs in success if obs.max_resident_bytes]
        ),
        rss_std_mb=std_or_zero(
            [bytes_to_mb(obs.max_resident_bytes) for obs in success if obs.max_resident_bytes]
        ),
        output_mean_mb=mean_or_none([bytes_to_mb(obs.primary_output_bytes) for obs in success]),
        output_std_mb=std_or_zero([bytes_to_mb(obs.primary_output_bytes) for obs in success]),
        lbd_triples=lbd_triples,
        ifcowl_triples=ifcowl_triples,
        notes=notes,
        observations=observations,
    )


def representative_path(result: AggregateResult, filename: str) -> Path | None:
    if not result.representative_run_dir:
        return None
    path = Path(result.representative_run_dir) / filename
    return path if path.exists() else None


def classify_outputs(run_dir: Path) -> dict[str, int]:
    sizes: dict[str, int] = {}
    for path in run_dir.iterdir():
        if not path.is_file():
            continue
        name = path.name.lower()
        size = path.stat().st_size
        if name == "out.ttl":
            sizes["lbd_ttl"] = size
        elif name == "out_ifcowl.ttl":
            sizes["ifcowl_ttl"] = size
        elif name.endswith(".trig"):
            sizes["trig_aux"] = sizes.get("trig_aux", 0) + size
        elif ".manifest.json" in name:
            sizes["manifest_json"] = sizes.get("manifest_json", 0) + size
        elif "-ifcowl.part-" in name and name.endswith(".nq"):
            sizes["ifcowl_nq"] = sizes.get("ifcowl_nq", 0) + size
        elif "-topology.part-" in name and name.endswith(".nq"):
            sizes["topology_nq"] = sizes.get("topology_nq", 0) + size
        elif "-lbd.part-" in name and name.endswith(".nq"):
            sizes["lbd_nq"] = sizes.get("lbd_nq", 0) + size
        elif name.endswith(".nq"):
            sizes["nq_other"] = sizes.get("nq_other", 0) + size
        else:
            sizes["other"] = sizes.get("other", 0) + size
    return sizes


def run_compare_if_possible(
    compare_bin: Path,
    left: Path | None,
    right: Path | None,
    left_base: str,
    right_base: str,
    extra: list[str],
) -> dict[str, str]:
    if not compare_bin.exists() or left is None or right is None:
        return {}
    completed = subprocess.run(
        [
            str(compare_bin),
            str(left),
            str(right),
            "--left-base",
            left_base,
            "--right-base",
            right_base,
            *extra,
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    summary: dict[str, str] = {
        "returncode": str(completed.returncode),
    }
    for line in completed.stdout.splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            if key in {
                "result",
                "left",
                "right",
                "missing_from_right",
                "missing_from_left",
                "identical",
                "total",
            }:
                summary[key] = value.strip()
    summary["stdout_excerpt"] = "\n".join(completed.stdout.splitlines()[:20])
    return summary


def count_turtle_triples(path: Path | None) -> int | None:
    if path is None or not path.exists():
        return None
    try:
        from rdflib import ConjunctiveGraph

        graph = ConjunctiveGraph()
        graph.parse(str(path), format="turtle")
        return len(graph)
    except Exception:
        return None


def parse_time_output(stderr: str) -> dict[str, float | int | None]:
    linux_wall = re.search(
        r"Elapsed \(wall clock\) time \(h:mm:ss or m:ss\):\s+([0-9:.\s]+)", stderr
    )
    linux_user = re.search(r"User time \(seconds\):\s+([0-9.]+)", stderr)
    linux_sys = re.search(r"System time \(seconds\):\s+([0-9.]+)", stderr)
    linux_rss = re.search(r"Maximum resident set size \(kbytes\):\s+([0-9]+)", stderr)

    if linux_wall:
        return {
            "wall_seconds": parse_elapsed_seconds(linux_wall.group(1).strip()),
            "user_seconds": float(linux_user.group(1)) if linux_user else None,
            "sys_seconds": float(linux_sys.group(1)) if linux_sys else None,
            "max_resident_bytes": int(linux_rss.group(1)) * 1024 if linux_rss else None,
        }

    mac_match = re.search(
        r"^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys$",
        stderr,
        re.MULTILINE,
    )
    mac_rss = re.search(r"^\s*([0-9]+)\s+maximum resident set size$", stderr, re.MULTILINE)
    return {
        "wall_seconds": float(mac_match.group(1)) if mac_match else None,
        "user_seconds": float(mac_match.group(2)) if mac_match else None,
        "sys_seconds": float(mac_match.group(3)) if mac_match else None,
        "max_resident_bytes": int(mac_rss.group(1)) if mac_rss else None,
    }


def parse_elapsed_seconds(value: str) -> float:
    parts = value.split(":")
    if len(parts) == 3:
        hours, minutes, seconds = parts
        return int(hours) * 3600 + int(minutes) * 60 + float(seconds)
    if len(parts) == 2:
        minutes, seconds = parts
        return int(minutes) * 60 + float(seconds)
    return float(value)


def mean_or_none(values: list[float | None]) -> float | None:
    numeric = [v for v in values if v is not None]
    if not numeric:
        return None
    return statistics.mean(numeric)


def std_or_zero(values: list[float | None]) -> float | None:
    numeric = [v for v in values if v is not None]
    if not numeric:
        return None
    if len(numeric) == 1:
        return 0.0
    return statistics.stdev(numeric)


def bytes_to_mb(value: int | None) -> float:
    return 0.0 if value is None else value / (1024 * 1024)


def render_markdown(report: dict[str, object]) -> str:
    cfg = report["configs"]
    baseline_keys = ["java_ttl_ifcowl", "rust_ttl_ifcowl"]
    mode_keys = [
        "rust_ttl_ifcowl",
        "rust_nquads_chunked",
        "rust_topology",
        "rust_topology_full_bbox",
    ]
    lines = [
        "# DigitalHub Benchmark Summary",
        "",
        f"- Model: `{report['model']['name']}` ({report['model']['megabytes']:.1f} MB)",
        f"- Repeats per configuration: `{report['repeats']}`",
        f"- Platform: `{report['host']['platform']}`",
        f"- CPU count: `{report['host']['cpu_count']}`",
        "",
        "## Baseline Comparison",
        "",
        "| Configuration | Successful runs | Wall mean ± sd (s) | Peak RSS mean ± sd (MB) | Output mean ± sd (MB) | LBD triples |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for key in baseline_keys:
        result = cfg[key]
        lines.append(
            "| {label} | {ok}/{repeats} | {wall} | {rss} | {out} | {triples} |".format(
                label=result["label"],
                ok=result["successful_runs"],
                repeats=result["repeats"],
                wall=fmt_mean_std(result["wall_mean"], result["wall_std"]),
                rss=fmt_mean_std(result["rss_mean_mb"], result["rss_std_mb"]),
                out=fmt_mean_std(result["output_mean_mb"], result["output_std_mb"]),
                triples=result["lbd_triples"] if result["lbd_triples"] is not None else "n/a",
            )
        )
    lines.extend(
        [
            "",
            "## Rust Mode Comparison",
            "",
            "| Configuration | Successful runs | Wall mean ± sd (s) | Peak RSS mean ± sd (MB) | Output mean ± sd (MB) |",
            "| --- | ---: | ---: | ---: | ---: |",
        ]
    )
    for key in mode_keys:
        result = cfg[key]
        lines.append(
            "| {label} | {ok}/{repeats} | {wall} | {rss} | {out} |".format(
                label=result["label"],
                ok=result["successful_runs"],
                repeats=result["repeats"],
                wall=fmt_mean_std(result["wall_mean"], result["wall_std"]),
                rss=fmt_mean_std(result["rss_mean_mb"], result["rss_std_mb"]),
                out=fmt_mean_std(result["output_mean_mb"], result["output_std_mb"]),
            )
        )
    if report["lbd_compare"]:
        lines.extend(
            [
                "",
                "## Normalized LBD Compare",
                "",
                "```text",
                report["lbd_compare"].get("stdout_excerpt", "").strip(),
                "```",
            ]
        )
    lines.extend(
        [
            "",
            "## Recommended Paper Assets",
            "",
            "- Table: DigitalHub repeated baseline and Rust mode comparison with mean ± sd for wall time, peak RSS, and output size.",
            "- Figure: baseline comparison plot with error bars for wall time, peak RSS, and output size.",
            "- Figure: Rust mode comparison plot with error bars for wall time, peak RSS, and output size.",
        ]
    )
    return "\n".join(lines) + "\n"


def fmt_mean_std(mean: float | None, std: float | None) -> str:
    if mean is None:
        return "n/a"
    if std is None:
        return f"{mean:.2f}"
    return f"{mean:.2f} ± {std:.2f}"


def write_plots(out_dir: Path, aggregates: dict[str, AggregateResult]) -> None:
    plot_baseline(out_dir / "digitalhub_baseline_stats.png", aggregates)
    plot_modes(out_dir / "digitalhub_mode_stats.png", aggregates)


def plot_baseline(path: Path, aggregates: dict[str, AggregateResult]) -> None:
    java = aggregates["java_ttl_ifcowl"]
    rust = aggregates["rust_ttl_ifcowl"]
    labels = ["Java", "Rust"]
    colors = [JAVA_GREY, RUST_RED]
    values_sets = [
        ([java.wall_mean or 0.0, rust.wall_mean or 0.0], [java.wall_std or 0.0, rust.wall_std or 0.0], "Wall Time", "seconds"),
        ([java.rss_mean_mb or 0.0, rust.rss_mean_mb or 0.0], [java.rss_std_mb or 0.0, rust.rss_std_mb or 0.0], "Peak RSS", "MB"),
        ([java.output_mean_mb or 0.0, rust.output_mean_mb or 0.0], [java.output_std_mb or 0.0, rust.output_std_mb or 0.0], "Output Size", "MB"),
    ]
    fig, axes = plt.subplots(1, 3, figsize=(12.5, 4.2))
    for ax, (vals, errs, title, ylabel) in zip(axes, values_sets):
        bars = ax.bar(labels, vals, yerr=errs, color=colors, capsize=5, alpha=0.95)
        ax.set_title(title)
        ax.set_ylabel(ylabel)
        ax.grid(axis="y", alpha=0.25)
        for bar, value in zip(bars, vals):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                value,
                f"{value:.2f}",
                ha="center",
                va="bottom",
                fontsize=9,
            )
    fig.suptitle("DigitalHub baseline comparison (mean ± sd)")
    fig.tight_layout()
    fig.savefig(path, dpi=180, bbox_inches="tight")
    plt.close(fig)


def plot_modes(path: Path, aggregates: dict[str, AggregateResult]) -> None:
    keys = [
        "rust_ttl_ifcowl",
        "rust_nquads_chunked",
        "rust_topology",
        "rust_topology_full_bbox",
    ]
    labels = ["TTL+IfcOWL", "NQ chunked", "Topology", "Full topo+bbox"]
    colors = [MODE_GREYS[0], MODE_REDS[0], MODE_GREYS[1], MODE_REDS[1]]
    values_sets = [
        (
            [aggregates[k].wall_mean or 0.0 for k in keys],
            [aggregates[k].wall_std or 0.0 for k in keys],
            "Wall Time by Mode",
            "seconds",
        ),
        (
            [aggregates[k].rss_mean_mb or 0.0 for k in keys],
            [aggregates[k].rss_std_mb or 0.0 for k in keys],
            "Peak RSS by Mode",
            "MB",
        ),
        (
            [aggregates[k].output_mean_mb or 0.0 for k in keys],
            [aggregates[k].output_std_mb or 0.0 for k in keys],
            "Output Size by Mode",
            "MB",
        ),
    ]
    fig, axes = plt.subplots(1, 3, figsize=(13.5, 4.5))
    for ax, (vals, errs, title, ylabel) in zip(axes, values_sets):
        bars = ax.bar(labels, vals, yerr=errs, color=colors, capsize=5, alpha=0.95)
        ax.set_title(title)
        ax.set_ylabel(ylabel)
        ax.tick_params(axis="x", rotation=16)
        ax.grid(axis="y", alpha=0.25)
        for bar, value in zip(bars, vals):
            ax.text(
                bar.get_x() + bar.get_width() / 2,
                value,
                f"{value:.2f}",
                ha="center",
                va="bottom",
                fontsize=8,
            )
    fig.suptitle("DigitalHub Rust mode comparison (mean ± sd)")
    fig.tight_layout()
    fig.savefig(path, dpi=180, bbox_inches="tight")
    plt.close(fig)


if __name__ == "__main__":
    main()
