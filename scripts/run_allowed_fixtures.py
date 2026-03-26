#!/usr/bin/env python3

import json
import platform
import re
import subprocess
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "artifacts" / "benchmarks"
BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
COMPARE_BIN = ROOT / "target" / "release" / "compare-turtle"
BASE_URIS = {
    "Duplex.ifc": "https://example.test/base/",
    "Infra-Bridge.ifc": "https://example.test/infra/",
    "IFC_SKW_Modell_07052019.ifc": "https://benchmark.test/bridge/",
}
FIXTURES = [
    ROOT / "Duplex.ifc",
    ROOT / "IFC_SKW_Modell_07052019.ifc",
    ROOT / "Infra-Bridge.ifc",
]


@dataclass
class ConversionResult:
    fixture: str
    fixture_bytes: int
    wall_seconds: float
    user_seconds: float
    sys_seconds: float
    max_resident_bytes: int | None
    lbd_bytes: int
    ifcowl_bytes: int
    lbd_file: str
    ifcowl_file: str


def main() -> None:
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    build_release_binaries()

    available_fixtures = [path for path in FIXTURES if path.exists()]
    if not available_fixtures:
        print("No allowed fixtures found locally; nothing to benchmark.")
        return

    missing = [path for path in FIXTURES if not path.exists()]
    for path in missing:
        print(f"Skipping missing fixture: {path}")

    conversions = [run_fixture(path) for path in available_fixtures]
    parity = run_parity_checks(conversions)
    topology = collect_topology_metrics(conversions)

    report = {
        "host": {
            "platform": platform.platform(),
            "cpu_count": cpu_count(),
        },
        "fixtures": [asdict(result) for result in conversions],
        "parity": parity,
        "topology": topology,
    }

    json_path = ARTIFACTS / "allowed_fixtures_report.json"
    md_path = ARTIFACTS / "allowed_fixtures_report.md"
    topology_snapshot_path = ARTIFACTS / "allowed_topology_snapshot.json"
    json_path.write_text(json.dumps(report, indent=2) + "\n")
    md_path.write_text(render_markdown(report))
    topology_snapshot_path.write_text(json.dumps(topology, indent=2) + "\n")

    print(f"Wrote {json_path}")
    print(f"Wrote {md_path}")
    print(f"Wrote {topology_snapshot_path}")


def build_release_binaries() -> None:
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


def run_fixture(path: Path) -> ConversionResult:
    slug = slugify(path.stem)
    lbd_out = ARTIFACTS / f"{slug}_allowed_lbd.ttl"
    ifcowl_out = ARTIFACTS / f"{slug}_allowed_lbd_ifcowl.ttl"

    for out in (lbd_out, ifcowl_out):
        if out.exists():
            out.unlink()

    base_uri = BASE_URIS[path.name]
    cmd = [
        "/usr/bin/time",
        "-l",
        str(BIN),
        str(path),
        "--output",
        str(lbd_out),
        "--ifcowl",
        "--base-uri",
        base_uri,
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
    return ConversionResult(
        fixture=path.name,
        fixture_bytes=path.stat().st_size,
        wall_seconds=timing["wall_seconds"],
        user_seconds=timing["user_seconds"],
        sys_seconds=timing["sys_seconds"],
        max_resident_bytes=timing["max_resident_bytes"],
        lbd_bytes=lbd_out.stat().st_size if lbd_out.exists() else 0,
        ifcowl_bytes=ifcowl_out.stat().st_size if ifcowl_out.exists() else 0,
        lbd_file=str(lbd_out.relative_to(ROOT)),
        ifcowl_file=str(ifcowl_out.relative_to(ROOT)),
    )


def run_parity_checks(conversions: list[ConversionResult]) -> dict[str, Any]:
    if not COMPARE_BIN.exists():
        return {"error": "compare-turtle binary missing"}

    by_fixture = {result.fixture: result for result in conversions}
    parity: dict[str, Any] = {}

    duplex = by_fixture["Duplex.ifc"]
    duplex_lbd_for_compare = merged_lbd_path(duplex)
    parity["duplex_lbd_normalized"] = run_compare(
        left=ROOT / "artifacts" / "reference-java" / "duplex_java_l3.ttl",
        right=duplex_lbd_for_compare,
        left_base=BASE_URIS["Duplex.ifc"],
        right_base=BASE_URIS["Duplex.ifc"],
        extra=["--normalize-lbd-opm"],
    )
    parity["duplex_ifcowl_normalized"] = run_compare(
        left=ROOT / "artifacts" / "reference-java" / "duplex_java_l3_ifcOWL.ttl",
        right=ROOT / duplex.ifcowl_file,
        left_base=BASE_URIS["Duplex.ifc"],
        right_base=BASE_URIS["Duplex.ifc"],
        extra=["--normalize-ifcowl-scalars"],
    )

    infra = by_fixture["Infra-Bridge.ifc"]
    infra_lbd_for_compare = merged_lbd_path(infra)
    parity["infra_lbd_normalized"] = run_compare(
        left=ROOT / "artifacts" / "test-output" / "infra_bridge_java_l3.ttl",
        right=infra_lbd_for_compare,
        left_base=BASE_URIS["Infra-Bridge.ifc"],
        right_base=BASE_URIS["Infra-Bridge.ifc"],
        extra=["--normalize-lbd-opm"],
    )
    parity["infra_ifcowl_normalized"] = run_compare(
        left=ROOT / "artifacts" / "test-output" / "infra_bridge_java_l3_ifcOWL.ttl",
        right=ROOT / infra.ifcowl_file,
        left_base=BASE_URIS["Infra-Bridge.ifc"],
        right_base=BASE_URIS["Infra-Bridge.ifc"],
        extra=["--normalize-ifcowl-scalars", "--sample-limit", "5"],
        timeout_seconds=180,
    )

    return parity


def collect_topology_metrics(conversions: list[ConversionResult]) -> dict[str, Any]:
    try:
        from rdflib import Graph, URIRef
    except Exception as exc:
        return {"error": f"rdflib unavailable: {exc}"}

    bot = "https://w3id.org/bot#"
    rdf_type = URIRef("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
    bot_contains_element = URIRef(bot + "containsElement")
    bot_adjacent_element = URIRef(bot + "adjacentElement")
    bot_has_sub_element = URIRef(bot + "hasSubElement")
    bot_building = URIRef(bot + "Building")
    bot_storey = URIRef(bot + "Storey")
    bot_space = URIRef(bot + "Space")
    bot_site = URIRef(bot + "Site")
    bot_element = URIRef(bot + "Element")

    metrics: dict[str, Any] = {}
    for result in conversions:
        lbd_path = ROOT / result.lbd_file
        if not lbd_path.exists():
            metrics[result.fixture] = {"error": f"missing lbd output: {lbd_path}"}
            continue

        graph = Graph()
        graph.parse(str(lbd_path), format="turtle")
        predicate_counts = Counter(
            str(predicate) for _, predicate, _ in graph.triples((None, None, None))
        )
        contains_edges = list(graph.triples((None, bot_contains_element, None)))
        adjacent_edges = list(graph.triples((None, bot_adjacent_element, None)))
        hosted_edges = list(graph.triples((None, bot_has_sub_element, None)))

        contains_out = Counter(str(subject) for subject, _, _ in contains_edges)
        adjacent_out = Counter(str(subject) for subject, _, _ in adjacent_edges)
        hosted_out = Counter(str(subject) for subject, _, _ in hosted_edges)

        def stats(counter: Counter[str]) -> dict[str, int]:
            if not counter:
                return {"min": 0, "median": 0, "max": 0}
            values = sorted(counter.values())
            return {
                "min": values[0],
                "median": values[len(values) // 2],
                "max": values[-1],
            }

        metrics[result.fixture] = {
            "triples": len(graph),
            "predicate_counts": dict(sorted(predicate_counts.items())),
            "nodes_from_roles": {
                "distinct_subjects": len({s for s, _, _ in graph.triples((None, None, None))}),
                "distinct_targets": len({o for _, _, o in graph.triples((None, None, None))}),
                "contains_subjects": len({s for s, _, _ in contains_edges}),
                "contains_targets": len({o for _, _, o in contains_edges}),
                "adjacent_subjects": len({s for s, _, _ in adjacent_edges}),
                "adjacent_targets": len({o for _, _, o in adjacent_edges}),
                "subelement_subjects": len({s for s, _, _ in hosted_edges}),
                "subelement_targets": len({o for _, _, o in hosted_edges}),
            },
            "edge_counts": {
                "containsElement": len(contains_edges),
                "adjacentElement": len(adjacent_edges),
                "hasSubElement": len(hosted_edges),
            },
            "out_degree": {
                "containsElement": stats(contains_out),
                "adjacentElement": stats(adjacent_out),
                "hasSubElement": stats(hosted_out),
            },
        }

    return metrics


def count_type(graph: Any, rdf_type: Any, class_iri: Any) -> int:
    return sum(1 for _ in graph.triples((None, rdf_type, class_iri)))


def merged_lbd_path(result: ConversionResult) -> Path:
    return ROOT / result.lbd_file


def run_compare(
    left: Path,
    right: Path,
    left_base: str,
    right_base: str,
    extra: list[str] | None = None,
    timeout_seconds: int = 120,
) -> dict[str, Any]:
    if not left.exists():
        return {"error": f"left file missing: {left}"}
    if not right.exists():
        return {"error": f"right file missing: {right}"}

    cmd = [
        str(COMPARE_BIN),
        "--left-base",
        left_base,
        "--right-base",
        right_base,
        *([*extra] if extra else []),
        str(left),
        str(right),
    ]
    try:
        completed = subprocess.run(
            cmd,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
            timeout=timeout_seconds,
        )
        summary = extract_compare_summary(completed.stdout)
        return {"summary": summary, "ok": True}
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": f"timeout after {timeout_seconds}s"}
    except subprocess.CalledProcessError as exc:
        return {
            "ok": False,
            "error": "compare command failed",
            "stderr_tail": tail(exc.stderr, 30),
            "stdout_tail": tail(exc.stdout, 30),
        }


def parse_time_output(stderr: str) -> dict[str, float | int | None]:
    real_match = re.search(
        r"^\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys$",
        stderr,
        re.MULTILINE,
    )
    if not real_match:
        raise RuntimeError(f"could not parse /usr/bin/time output:\n{stderr}")

    rss_match = re.search(
        r"^\s*([0-9]+)\s+maximum resident set size$",
        stderr,
        re.MULTILINE,
    )
    max_rss = int(rss_match.group(1)) if rss_match else None

    return {
        "wall_seconds": float(real_match.group(1)),
        "user_seconds": float(real_match.group(2)),
        "sys_seconds": float(real_match.group(3)),
        "max_resident_bytes": max_rss,
    }


def extract_compare_summary(stdout: str) -> dict[str, str]:
    summary: dict[str, str] = {}
    for line in stdout.splitlines():
        if line.startswith("left: "):
            summary["left"] = line
        elif line.startswith("right: "):
            summary["right"] = line
        elif line.startswith("missing_from_right="):
            summary["missing_from_right"] = line.split("=", 1)[1]
        elif line.startswith("missing_from_left="):
            summary["missing_from_left"] = line.split("=", 1)[1]
        elif line.startswith("result="):
            summary["result"] = line.split("=", 1)[1]
    return summary


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Allowed Fixtures Report",
        "",
        "Only these fixtures are included:",
        "- `Duplex.ifc`",
        "- `IFC_SKW_Modell_07052019.ifc`",
        "- `Infra-Bridge.ifc`",
        "",
        f"- Platform: `{report['host']['platform']}`",
        f"- CPU count: `{report['host']['cpu_count']}`",
        "",
        "| Fixture | Size (MB) | Wall (s) | Max RSS (MB) | LBD (MB) | IfcOWL (MB) |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]

    for item in report["fixtures"]:
        lines.append(
            "| {fixture} | {size_mb:.1f} | {wall:.2f} | {rss_mb:.1f} | {lbd_mb:.1f} | {ifcowl_mb:.1f} |".format(
                fixture=item["fixture"],
                size_mb=item["fixture_bytes"] / (1024 * 1024),
                wall=item["wall_seconds"],
                rss_mb=(item["max_resident_bytes"] or 0) / (1024 * 1024),
                lbd_mb=item["lbd_bytes"] / (1024 * 1024),
                ifcowl_mb=item["ifcowl_bytes"] / (1024 * 1024),
            )
        )

    lines.extend(["", "## Parity", ""])
    for key, value in report["parity"].items():
        lines.append(f"### {key}")
        if value.get("ok"):
            summary = value.get("summary", {})
            lines.append(f"- {summary.get('left', 'left: n/a')}")
            lines.append(f"- {summary.get('right', 'right: n/a')}")
            lines.append(f"- missing_from_right={summary.get('missing_from_right', 'n/a')}")
            lines.append(f"- missing_from_left={summary.get('missing_from_left', 'n/a')}")
            if "result" in summary:
                lines.append(f"- result={summary['result']}")
        else:
            lines.append(f"- error: {value.get('error', 'unknown error')}")
        lines.append("")

    lines.extend(["## Topology (in LBD output)", ""])
    topology = report.get("topology", {})
    if isinstance(topology, dict) and "error" in topology:
        lines.append(f"- error: {topology['error']}")
    else:
        for fixture, metric in topology.items():
            lines.append(f"### {fixture}")
            if "error" in metric:
                lines.append(f"- error: {metric['error']}")
                lines.append("")
                continue
            edge_counts = metric["edge_counts"]
            lines.append(
                "- edges: containsElement={contains}, adjacentElement={adjacent}, hasSubElement={hosted}".format(
                    contains=edge_counts["containsElement"],
                    adjacent=edge_counts["adjacentElement"],
                    hosted=edge_counts["hasSubElement"],
                )
            )
            contains_degree = metric["out_degree"]["containsElement"]
            adjacent_degree = metric["out_degree"]["adjacentElement"]
            lines.append(
                "- out-degree containsElement min/median/max: {min}/{median}/{max}".format(
                    min=contains_degree["min"],
                    median=contains_degree["median"],
                    max=contains_degree["max"],
                )
            )
            lines.append(
                "- out-degree adjacentElement min/median/max: {min}/{median}/{max}".format(
                    min=adjacent_degree["min"],
                    median=adjacent_degree["median"],
                    max=adjacent_degree["max"],
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
    output = subprocess.run(
        ["getconf", "_NPROCESSORS_ONLN"],
        capture_output=True,
        text=True,
        check=True,
    )
    return int(output.stdout.strip())


def slugify(value: str) -> str:
    value = value.lower()
    value = re.sub(r"[^a-z0-9]+", "_", value)
    return value.strip("_")


def tail(text: str, line_count: int) -> str:
    lines = text.splitlines()
    return "\n".join(lines[-line_count:])


if __name__ == "__main__":
    main()
