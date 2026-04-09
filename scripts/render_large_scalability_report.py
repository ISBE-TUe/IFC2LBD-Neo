#!/usr/bin/env python3

from __future__ import annotations

import json
import re
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "artifacts" / "paper-benchmarks"
CORES = [1, 2, 4, 8, 16]
REPRESENTATIVE_CORE = 4

SERIES = [
    {
        "key": "plain_ttl",
        "label": "LBD + IfcOWL (TTL)",
        "directory": ARTIFACTS / "large_scalability_ttl",
        "suffix": "ttl",
        "syntax": "ttl",
        "mode": "plain",
        "color": "#6C7075",
        "linestyle": "-",
    },
    {
        "key": "plain_nq",
        "label": "LBD + IfcOWL (N-Q, chunked)",
        "directory": ARTIFACTS / "large_scalability",
        "suffix": "nq",
        "syntax": "nq",
        "mode": "plain",
        "color": "#6C7075",
        "linestyle": "--",
    },
    {
        "key": "topology_ttl",
        "label": "--topology (TTL)",
        "directory": ARTIFACTS / "large_scalability_topology_ttl",
        "suffix": "ttl",
        "syntax": "ttl",
        "mode": "topology",
        "color": "#8D6262",
        "linestyle": "-",
    },
    {
        "key": "topology_nq",
        "label": "--topology (N-Q, chunked)",
        "directory": ARTIFACTS / "large_scalability_topology",
        "suffix": "nq",
        "syntax": "nq",
        "mode": "topology",
        "color": "#8D6262",
        "linestyle": "--",
    },
    {
        "key": "full_topology_ttl",
        "label": "--topology-full (TTL)",
        "directory": ARTIFACTS / "large_scalability_full_topology_ttl",
        "suffix": "ttl",
        "syntax": "ttl",
        "mode": "full_topology",
        "color": "#B58F8F",
        "linestyle": "-",
    },
    {
        "key": "full_topology_nq",
        "label": "--topology-full (N-Q, chunked)",
        "directory": ARTIFACTS / "large_scalability_full_topology",
        "suffix": "nq",
        "syntax": "nq",
        "mode": "full_topology",
        "color": "#B58F8F",
        "linestyle": "--",
    },
]

PANEL_ORDER = [
    "plain_ttl",
    "plain_nq",
    "topology_ttl",
    "topology_nq",
    "full_topology_ttl",
    "full_topology_nq",
]
PANEL_LABELS = [
    "Plain\nTTL",
    "Plain\nN-Q",
    "Topo\nTTL",
    "Topo\nN-Q",
    "Full\nTTL",
    "Full\nN-Q",
]
PANEL_COLORS = [next(item["color"] for item in SERIES if item["key"] == key) for key in PANEL_ORDER]


def main() -> None:
    summary = build_summary()
    out_json = ARTIFACTS / "large_model_scalability_summary.json"
    out_md = ARTIFACTS / "large_model_scalability_story.md"
    out_json.write_text(json.dumps(summary, indent=2) + "\n")
    out_md.write_text(render_markdown(summary))
    plot_wall_time(summary, ARTIFACTS / "large_model_scalability_wall.png")
    plot_mode_metrics(summary, ARTIFACTS / "large_model_scalability_metrics.png")
    plot_combined(summary, ARTIFACTS / "large_model_scalability_2x2.png")


def build_summary() -> dict[str, object]:
    series_summary: dict[str, object] = {}
    for spec in SERIES:
        summary_path = spec["directory"] / "summary.json"
        summary_data = json.loads(summary_path.read_text()) if summary_path.exists() else {}
        runs: dict[str, object] = {}
        for core in CORES:
            run_key = f"{core}core_{spec['suffix']}"
            run_dir = spec["directory"] / run_key
            if run_key not in summary_data:
                continue
            runs[str(core)] = normalize_summary_run(summary_data[run_key], run_dir, spec["syntax"])
        series_summary[spec["key"]] = {
            "label": spec["label"],
            "syntax": spec["syntax"],
            "mode": spec["mode"],
            "color": spec["color"],
            "linestyle": spec["linestyle"],
            "directory": str(spec["directory"]),
            "runs": runs,
        }

    digitalhub_chunk = load_digitalhub_chunk_metadata()
    return {
        "model": {
            "name": "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc",
            "path": str(ROOT / "CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc"),
        },
        "hardware": {
            "paper_label": "Debian 12, AMD 9955HX, 64 GB RAM",
            "recorded_host": "Linux-6.1.0-44-amd64-x86_64-with-glibc2.36",
        },
        "series": series_summary,
        "digitalhub_chunk_metadata": digitalhub_chunk,
        "takeaways": derive_takeaways(series_summary),
    }


def normalize_summary_run(data: dict[str, object], run_dir: Path, syntax: str) -> dict[str, object]:
    wall_text = str(data["wall"])
    max_rss_mb = (
        float(data["max_rss_mb"])
        if "max_rss_mb" in data
        else int(data["max_rss_kbytes"]) / 1024.0
    )
    avail_before_gb = float(data["avail_before_gb"]) if "avail_before_gb" in data else None
    avail_after_gb = float(data["avail_after_gb"]) if "avail_after_gb" in data else None
    disk_delta_gb = float(data["delta_gb"]) if "delta_gb" in data else None

    if syntax == "nq":
        graph_sizes = graph_sizes_gb(run_dir)
        chunk_breakdown = manifest_chunk_breakdown(run_dir)
        total_output_gb = sum(graph_sizes.values())
        chunk_files = sum(item["files"] for item in chunk_breakdown.values())
        file_sizes = {}
    else:
        lbd_bytes = int(data.get("lbd_ttl_bytes", 0))
        ifcowl_bytes = int(data.get("ifcowl_ttl_bytes", 0))
        total_output_bytes = int(data.get("total_output_bytes", lbd_bytes + ifcowl_bytes))
        total_output_gb = total_output_bytes / 1024.0 / 1024.0 / 1024.0
        graph_sizes = {"lbd": 0.0, "ifcowl": 0.0, "topology": 0.0}
        chunk_breakdown = {}
        chunk_files = 0
        file_sizes = {
            "lbd_ttl_gb": lbd_bytes / 1024.0 / 1024.0 / 1024.0,
            "ifcowl_ttl_gb": ifcowl_bytes / 1024.0 / 1024.0 / 1024.0,
        }

    if disk_delta_gb is None:
        disk_delta_gb = total_output_gb

    return {
        "returncode": int(data["returncode"]),
        "wall_time_text": wall_text,
        "wall_seconds": parse_elapsed_seconds(wall_text),
        "user_seconds": float(data["user_seconds"]),
        "sys_seconds": float(data["sys_seconds"]),
        "max_rss_mb": max_rss_mb,
        "total_output_gb": total_output_gb,
        "chunk_files": chunk_files,
        "chunk_breakdown": chunk_breakdown,
        "graph_sizes_gb": graph_sizes,
        "file_sizes_gb": file_sizes,
        "avail_before_gb": avail_before_gb,
        "avail_after_gb": avail_after_gb,
        "disk_delta_gb": disk_delta_gb,
        "threads_label": data.get("threads_label", ""),
        "run_dir": str(run_dir),
    }


def graph_sizes_gb(run_dir: Path) -> dict[str, float]:
    result = {"lbd": 0.0, "ifcowl": 0.0, "topology": 0.0}
    for key in result:
        manifest = run_dir / f"large-{key}.manifest.json"
        if manifest.exists():
            data = json.loads(manifest.read_text())
            result[key] = sum(item["bytes"] for item in data.get("files", [])) / 1024.0 / 1024.0 / 1024.0
    return result


def manifest_chunk_breakdown(run_dir: Path) -> dict[str, dict[str, int]]:
    result: dict[str, dict[str, int]] = {}
    for manifest in sorted(run_dir.glob("*.manifest.json")):
        data = json.loads(manifest.read_text())
        files = data.get("files", [])
        result[manifest.name] = {
            "files": len(files),
            "bytes": sum(item["bytes"] for item in files),
            "total_lines": int(data.get("total_lines", 0)),
            "total_triples_estimate": int(data.get("total_triples_estimate", 0)),
            "core_chunk_count": int(data.get("core_chunk_count", 0)),
            "chunk_size_lines": int(data.get("chunk_size_lines", 0)),
            "chunk_size_bytes": int(data.get("chunk_size_bytes", 0)),
        }
    return result


def derive_takeaways(series_summary: dict[str, object]) -> dict[str, object]:
    takeaways: dict[str, object] = {}
    for key, series in series_summary.items():
        ordered = [(int(core), item["wall_seconds"]) for core, item in series["runs"].items()]
        ordered.sort()
        if not ordered:
            continue
        best_core, best_wall = min(ordered, key=lambda item: item[1])
        baseline = dict(ordered).get(1)
        takeaways[key] = {
            "best_core": best_core,
            "best_wall_seconds": best_wall,
            "speedup_vs_1core": None if baseline is None else baseline / best_wall,
        }
    return takeaways


def load_digitalhub_chunk_metadata() -> dict[str, object]:
    path = ARTIFACTS / "digitalhub_chunk_metadata" / "run_01" / "chunk_summary.json"
    return json.loads(path.read_text()) if path.exists() else {}


def render_markdown(summary: dict[str, object]) -> str:
    lines = [
        "# Large-Model Scalability Summary",
        "",
        f"- Model: `{summary['model']['name']}`",
        f"- Hardware label: `{summary['hardware']['paper_label']}`",
        f"- Recorded host: `{summary['hardware']['recorded_host']}`",
        "",
        "## Main Findings",
        "",
    ]

    takeaways = summary["takeaways"]
    plain_ttl_best = takeaways["plain_ttl"]["best_core"]
    plain_nq_best = takeaways["plain_nq"]["best_core"]
    top_ttl_best = takeaways["topology_ttl"]["best_core"]
    top_nq_best = takeaways["topology_nq"]["best_core"]
    full_ttl_best = takeaways["full_topology_ttl"]["best_core"]
    full_nq_best = takeaways["full_topology_nq"]["best_core"]
    plain_ttl_4 = summary["series"]["plain_ttl"]["runs"]["4"]
    plain_nq_4 = summary["series"]["plain_nq"]["runs"]["4"]
    top_ttl_4 = summary["series"]["topology_ttl"]["runs"]["4"]
    top_nq_4 = summary["series"]["topology_nq"]["runs"]["4"]
    full_ttl_4 = summary["series"]["full_topology_ttl"]["runs"]["4"]
    full_nq_4 = summary["series"]["full_topology_nq"]["runs"]["4"]

    lines.extend(
        [
            f"- For plain `LBD + IfcOWL`, `TTL` is clearly cheaper than chunked `N-Quads`: at `4 cores / 8 threads`, `TTL` is `{plain_ttl_4['wall_seconds']:.2f} s` and `{plain_ttl_4['max_rss_mb']:.0f} MB`, versus `{plain_nq_4['wall_seconds']:.2f} s` and `{plain_nq_4['max_rss_mb']:.0f} MB` for `N-Quads`.",
            f"- The same is true for `--topology`: at `4 cores / 8 threads`, `TTL` is `{top_ttl_4['wall_seconds']:.2f} s` and `{top_ttl_4['max_rss_mb']:.0f} MB`, versus `{top_nq_4['wall_seconds']:.2f} s` and `{top_nq_4['max_rss_mb']:.0f} MB` for chunked `N-Quads`.",
            f"- `--topology-full` behaves differently: chunked `N-Quads` is faster at `4 cores / 8 threads` (`{full_nq_4['wall_seconds']:.2f} s`) than `TTL` (`{full_ttl_4['wall_seconds']:.2f} s`), so the format effect is not monotonic once the exact-kernel topology work dominates.",
            f"- Adding `2 cores / 4 threads` improves the large-model curves materially. For example, plain `TTL` drops from `{summary['series']['plain_ttl']['runs']['1']['wall_seconds']:.2f} s` at `1 core` to `{summary['series']['plain_ttl']['runs']['2']['wall_seconds']:.2f} s` at `2 cores / 4 threads`, then only slightly further to `{plain_ttl_4['wall_seconds']:.2f} s` at `4 cores / 8 threads`.",
            f"- Best observed core settings in this environment are: plain `TTL` at `{plain_ttl_best}` cores, plain `N-Quads` at `{plain_nq_best}` cores, `--topology (TTL)` at `{top_ttl_best}` cores, `--topology (N-Quads)` at `{top_nq_best}` cores, `--topology-full (TTL)` at `{full_ttl_best}` cores, and `--topology-full (N-Quads)` at `{full_nq_best}` cores.",
            "",
            "## Results",
            "",
            "| Series | Configuration | Wall time (s) | Peak RSS (MB) | Output (GB) | Chunk files |",
            "| --- | --- | ---: | ---: | ---: | ---: |",
        ]
    )

    for spec in SERIES:
        series = summary["series"][spec["key"]]
        for core in CORES:
            run = series["runs"].get(str(core))
            if not run:
                continue
            config_label = f"{core} core / 2 threads" if core == 1 else f"{core} cores / {core * 2} threads"
            lines.append(
                f"| {series['label']} | {config_label} | {run['wall_seconds']:.2f} | {run['max_rss_mb']:.2f} | {run['total_output_gb']:.3f} | {run['chunk_files']} |"
            )

    lines.extend(
        [
            "",
            "## Chunk Metadata",
            "",
            "### DigitalHub Representative Chunked N-Quads Run",
            "",
            "| Graph family | Chunk files | Bytes | Lines / triples | Core chunk count |",
            "| --- | ---: | ---: | ---: | ---: |",
        ]
    )

    for manifest_name, item in summary["digitalhub_chunk_metadata"].items():
        if manifest_name == "timing":
            continue
        graph_family = manifest_name.replace("digitalhub-", "").replace(".manifest.json", "")
        lines.append(
            f"| `{graph_family}` | {item['files']} | {item['bytes']} | {item['total_lines']} | {item['core_chunk_count']} |"
        )

    lines.extend(
        [
            "",
            f"- DigitalHub chunk policy: `cores`, `chunk_size_lines = {summary['digitalhub_chunk_metadata']['digitalhub-lbd.manifest.json']['chunk_size_lines']}`, `chunk_size_bytes = {summary['digitalhub_chunk_metadata']['digitalhub-lbd.manifest.json']['chunk_size_bytes']}`.",
            "",
            "### Large-Model Chunked N-Quads Breakdown",
            "",
            "| Series | Core setting | LBD chunks | IfcOWL chunks | Topology chunks | Total chunks |",
            "| --- | --- | ---: | ---: | ---: | ---: |",
        ]
    )

    for series_key in ["plain_nq", "topology_nq", "full_topology_nq"]:
        series = summary["series"][series_key]
        for core in CORES:
            run = series["runs"].get(str(core))
            if not run:
                continue
            chunk_breakdown = run["chunk_breakdown"]
            lbd = chunk_breakdown.get("large-lbd.manifest.json", {}).get("files", 0)
            ifcowl = chunk_breakdown.get("large-ifcowl.manifest.json", {}).get("files", 0)
            topology = chunk_breakdown.get("large-topology.manifest.json", {}).get("files", 0)
            config_label = f"{core} core / 2 threads" if core == 1 else f"{core} cores / {core * 2} threads"
            lines.append(f"| {series['label']} | {config_label} | {lbd} | {ifcowl} | {topology} | {run['chunk_files']} |")

    lines.extend(
        [
            "",
            "## Method Notes",
            "",
            "- DigitalHub remains the repeated benchmark section with `n = 20` and `μ ± σ` reporting.",
            "- The large-model scalability matrix is currently a single-run study per configuration (`n = 1`). This is acceptable for the paper if the text presents it as a controlled scalability sweep rather than a variance study.",
            "- If stronger statistical backing is needed on the large model, the next sensible step is selective repetition such as `n = 3` for representative series, not `n = 20` for the full matrix.",
            "- `TTL` and chunked `N-Quads` are not directly size-comparable in a naïve way. `N-Quads` repeats the named-graph IRI on every line, which heavily inflates the IfcOWL side.",
            "- For `TTL`, topology triples are folded into the LBD file. Therefore, the split `LBD / IfcOWL / Topology` family breakdown is only cleanly available for chunked `N-Quads`.",
            "- Small RSS reorderings between plain and `--topology` should not be overinterpreted. The topology graph is tiny in the lightweight mode, so peak RSS is dominated by the shared conversion and writing pipeline.",
            "",
            "## Plot Guidance",
            "",
            "- Use the wall-time plot as the main scalability figure. It now shows all six series across `1 / 2 / 4 / 8 / 16` cores.",
            "- Use the companion three-panel figure as a format-and-mode comparison at `4 cores / 8 threads`.",
            "- In the paper text, explicitly state that the `N-Quads` series are chunked `N-Quads` and the `TTL` series are the plain Turtle outputs with IfcOWL sidecar.",
        ]
    )
    return "\n".join(lines) + "\n"


def plot_wall_time(summary: dict[str, object], path: Path) -> None:
    fig, ax = plt.subplots(figsize=(7.4, 4.8))
    draw_wall_panel(summary, ax)
    fig.tight_layout()
    fig.savefig(path, dpi=180, bbox_inches="tight")
    plt.close(fig)


def plot_mode_metrics(summary: dict[str, object], path: Path) -> None:
    fig, axes = plt.subplots(1, 3, figsize=(13.6, 4.2))
    draw_metrics_panels(summary, axes)
    fig.subplots_adjust(bottom=0.22, wspace=0.32)
    fig.savefig(path, dpi=180, bbox_inches="tight")
    plt.close(fig)


def draw_wall_panel(summary: dict[str, object], ax: plt.Axes) -> None:
    for spec in SERIES:
        series = summary["series"][spec["key"]]
        xs = []
        ys = []
        for core in CORES:
            run = series["runs"].get(str(core))
            if run:
                xs.append(core)
                ys.append(run["wall_seconds"])
        ax.plot(
            xs,
            ys,
            marker="o",
            linewidth=2.4,
            markersize=5,
            color=spec["color"],
            linestyle="-" if spec["syntax"] == "ttl" else (0, (6, 3)),
            label=spec["label"],
        )
    ax.set_title("Large-Model Wall Time by Core Count")
    ax.set_xlabel("CPU Cores")
    ax.set_ylabel("Wall Time (s)")
    ax.set_xticks(CORES)
    ax.grid(axis="y", alpha=0.25)
    ax.set_xlim(0.7, 16.3)
    ax.legend(
        frameon=True,
        fontsize=7,
        ncol=2,
        loc="upper center",
        bbox_to_anchor=(0.5, 0.98),
        handlelength=3.0,
        columnspacing=1.0,
        handletextpad=0.6,
        borderpad=0.4,
        labelspacing=0.4,
    )


def draw_metrics_panels(summary: dict[str, object], axes: list[plt.Axes] | tuple[plt.Axes, ...]) -> None:
    wall_values = []
    rss_values = []
    out_values = []
    for key in PANEL_ORDER:
        run = summary["series"][key]["runs"][str(REPRESENTATIVE_CORE)]
        wall_values.append(run["wall_seconds"])
        rss_values.append(run["max_rss_mb"])
        out_values.append(run["total_output_gb"])

    for ax, values, title, ylabel in [
        (axes[0], wall_values, "Wall Time at 4 cores / 8 threads", "s"),
        (axes[1], rss_values, "Peak RSS at 4 cores / 8 threads", "MB"),
        (axes[2], out_values, "Output Size at 4 cores / 8 threads", "GB"),
    ]:
        bars = ax.bar(PANEL_LABELS, values, color=PANEL_COLORS, alpha=0.95)
        ax.set_title(title)
        ax.set_ylabel(ylabel)
        ax.grid(axis="y", alpha=0.25)
        for bar, value in zip(bars, values):
            label = f"{value:.1f}" if ylabel == "s" else (f"{value:.0f}" if ylabel == "MB" else f"{value:.2f}")
            ax.text(bar.get_x() + bar.get_width() / 2, value, label, ha="center", va="bottom", fontsize=8)
        ax.tick_params(axis="x", rotation=0)


def plot_combined(summary: dict[str, object], path: Path) -> None:
    fig, axes = plt.subplots(2, 2, figsize=(13.6, 8.4))
    draw_wall_panel(summary, axes[0, 0])
    draw_metrics_panels(summary, [axes[0, 1], axes[1, 0], axes[1, 1]])
    fig.subplots_adjust(bottom=0.16, wspace=0.28, hspace=0.32)
    fig.savefig(path, dpi=180, bbox_inches="tight")
    plt.close(fig)


def parse_elapsed_seconds(value: str) -> float:
    parts = value.split(":")
    if len(parts) == 3:
        hours, minutes, seconds = parts
        return int(hours) * 3600 + int(minutes) * 60 + float(seconds)
    if len(parts) == 2:
        minutes, seconds = parts
        return int(minutes) * 60 + float(seconds)
    return float(value)


if __name__ == "__main__":
    main()
