#!/usr/bin/env python3
# On macOS with Homebrew: use `/opt/homebrew/bin/python3` if ifcopenshell/rdflib
# are not on the system python. Example: python3.14 scripts/validate_conversion.py
"""
Validate ifc2lbd-neo conversion completeness and correctness.

Runs the CLI binary on IFC files ≤15 MB, uses IfcOpenShell as ground truth,
then verifies the converted LBD (BOT/BEO/OPM) Turtle graph with rdflib.

Usage:
    python3 scripts/validate_conversion.py
    python3 scripts/validate_conversion.py path/to/model.ifc ...

Requirements:
    pip install ifcopenshell rdflib

The binary must be built first:
    cargo build --release -p ifc2lbd-cli
"""

import json
import random
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional

import ifcopenshell
import rdflib
from rdflib import OWL, RDF, RDFS, Graph, Literal, Namespace, URIRef

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
BASE_URI = "https://validate.test/"
MAX_FILE_BYTES = 15 * 1024 * 1024
SPOT_SAMPLE_SIZE = 20
RANDOM_SEED = 42

BOT = Namespace("https://w3id.org/bot#")
LBD = Namespace("https://linkedbuildingdata.org/LBD#")
OPM_NS = Namespace("https://w3id.org/opm#")


# ── GUID helpers ──────────────────────────────────────────────────────────────
# IRIs use the raw 22-char IFC GUID as the suffix, e.g.:
#   https://validate.test/wall_1DnXIsP9rDi8TggM6J4GkL
# IFC GUIDs may contain '_' and '$' (the IFC base64 alphabet), so we
# cannot use rsplit("_") to extract them — we always take the last 22 chars.


def guid_from_iri(iri: str) -> str:
    """Extract the 22-char IFC GUID from an IRI like …/wall_1DnXIsP9rDi8TggM6J4GkL."""
    path = iri.rsplit("/", 1)[-1]
    return path[-22:] if len(path) >= 22 else path


# ── IFC ground truth (IfcOpenShell) ───────────────────────────────────────────


def collect_ifc_facts(ifc_path: Path, rng: random.Random) -> dict:
    ifc = ifcopenshell.open(str(ifc_path))

    spatial_types = {
        "project": ifc.by_type("IfcProject"),
        "site": ifc.by_type("IfcSite"),
        "building": ifc.by_type("IfcBuilding"),
        "storey": ifc.by_type("IfcBuildingStorey"),
        "space": ifc.by_type("IfcSpace"),
    }

    all_elements = ifc.by_type("IfcElement")
    elements_by_type: dict[str, int] = {}
    for el in all_elements:
        t = el.is_a()
        elements_by_type[t] = elements_by_type.get(t, 0) + 1

    all_psets = ifc.by_type("IfcPropertySet")
    single_values = ifc.by_type("IfcPropertySingleValue")
    enum_values = ifc.by_type("IfcPropertyEnumeratedValue")
    containment_rels = ifc.by_type("IfcRelContainedInSpatialStructure")

    # Count only psets the converter can actually emit. Excluded:
    #   - IfcRelDefinesByProperties with empty RelatedObjects (invalid IFC)
    #   - RelatedObjects that are IfcZone/IfcGroup (not modeled as spatial nodes)
    #   - Type-owned psets where the type has zero instances
    reachable_psets: set[int] = set()
    for rel in ifc.by_type("IfcRelDefinesByProperties"):
        objects = list(rel.RelatedObjects)
        if not objects:
            continue
        pdef = rel.RelatingPropertyDefinition
        if not pdef or not pdef.is_a("IfcPropertySet"):
            continue
        for obj in objects:
            if (obj.is_a("IfcElement")
                    or obj.is_a("IfcSpatialStructureElement")
                    or obj.is_a("IfcProject")):
                reachable_psets.add(pdef.id())
                break
    types_with_instances: set[int] = set()
    for rel in ifc.by_type("IfcRelDefinesByType"):
        if list(rel.RelatedObjects):
            types_with_instances.add(rel.RelatingType.id())
    for type_obj in ifc.by_type("IfcTypeObject"):
        if type_obj.id() not in types_with_instances:
            continue
        if getattr(type_obj, "HasPropertySets", None):
            for ps in type_obj.HasPropertySets:
                if ps.is_a("IfcPropertySet"):
                    reachable_psets.add(ps.id())

    # Element spot-check sample
    element_sample = []
    for el in rng.sample(all_elements, min(SPOT_SAMPLE_SIZE, len(all_elements))):
        try:
            element_sample.append({
                "guid": el.GlobalId,
                "type": el.is_a(),
                "name": el.Name,
            })
        except Exception:
            pass

    # Containment spot-check sample
    containment_sample = []
    for rel in rng.sample(containment_rels, min(SPOT_SAMPLE_SIZE * 2, len(containment_rels))):
        if len(containment_sample) >= SPOT_SAMPLE_SIZE:
            break
        structure = rel.RelatingStructure
        elements = list(rel.RelatedElements)
        if not elements:
            continue
        el = elements[0]
        try:
            containment_sample.append({
                "element_guid": el.GlobalId,
                "element_type": el.is_a(),
                "structure_guid": structure.GlobalId,
                "structure_type": structure.is_a(),
            })
        except Exception:
            pass

    # Property value spot-check sample
    prop_sample = []
    candidates = [p for p in single_values if p.NominalValue is not None]
    for p in rng.sample(candidates, min(SPOT_SAMPLE_SIZE, len(candidates))):
        try:
            prop_sample.append({
                "name": p.Name,
                "raw_value": str(p.NominalValue.wrappedValue),
                "ifc_type": p.NominalValue.is_a(),
            })
        except Exception:
            pass

    return {
        "schema": ifc.schema,
        "spatial_counts": {k: len(v) for k, v in spatial_types.items()},
        "element_count": len(all_elements),
        "elements_by_type": elements_by_type,
        "pset_count": len(all_psets),
        "reachable_pset_count": len(reachable_psets),
        "single_value_count": len(single_values),
        "enum_value_count": len(enum_values),
        "containment_rel_count": len(containment_rels),
        "element_sample": element_sample,
        "containment_sample": containment_sample,
        "prop_sample": prop_sample,
    }


# ── CLI conversion ─────────────────────────────────────────────────────────────


LBD_MODULES = [
    "neo-bot-producer",
    "neo-beo-producer",
    "neo-props-opm",
    "neo-turtle-serializer",
    "neo-file-export",
]


def run_conversion(ifc_path: Path, out_ttl: Path) -> float:
    args = [str(BIN), str(ifc_path), "-o", str(out_ttl), "-u", BASE_URI]
    for mod in LBD_MODULES:
        args += ["--module", mod]
    t0 = time.monotonic()
    result = subprocess.run(
        args,
        capture_output=True,
        text=True,
        timeout=300,
    )
    elapsed = time.monotonic() - t0
    if result.returncode != 0:
        raise RuntimeError(f"CLI exited {result.returncode}:\n{result.stderr[:3000]}")
    return elapsed


# ── RDF graph analysis (rdflib) ───────────────────────────────────────────────


def collect_graph_facts(ttl_path: Path) -> dict:
    g = Graph()
    g.parse(str(ttl_path), format="turtle")

    def count(rdf_type):
        return sum(1 for _ in g.subjects(RDF.type, rdf_type))

    spatial_counts = {
        "project": count(LBD.Project),
        "site": count(BOT.Site),
        "building": count(BOT.Building),
        "storey": count(BOT.Storey),
        "space": count(BOT.Space),
    }

    elements = set(g.subjects(RDF.type, BOT.Element))
    element_count = len(elements)

    # GUID → element IRI (last 22 chars of IRI path = raw IFC GUID)
    guid_to_element: dict[str, str] = {}
    for s in elements:
        iri = str(s)
        token = guid_from_iri(iri)
        if token:
            guid_to_element[token] = iri

    # GUID → spatial IRI
    spatial_iris: set[URIRef] = set()
    for cls in [LBD.Project, BOT.Site, BOT.Building, BOT.Storey, BOT.Space, BOT.Zone]:
        spatial_iris.update(g.subjects(RDF.type, cls))
    guid_to_spatial: dict[str, str] = {}
    for s in spatial_iris:
        iri = str(s)
        token = guid_from_iri(iri)
        if token:
            guid_to_spatial[token] = iri

    pset_count = count(LBD.PropertySet)
    property_count = count(OPM_NS.Property)
    containment_count = sum(1 for _ in g.triples((None, BOT.containsElement, None)))

    return {
        "spatial_counts": spatial_counts,
        "element_count": element_count,
        "guid_to_element": guid_to_element,
        "guid_to_spatial": guid_to_spatial,
        "pset_count": pset_count,
        "property_count": property_count,
        "containment_count": containment_count,
        "graph": g,
    }


# ── Spot checks ───────────────────────────────────────────────────────────────


def check_guid_iris(
    ifc_facts: dict, graph_facts: dict
) -> tuple[int, int, list[str]]:
    ok = total = 0
    failures: list[str] = []
    guid_map = graph_facts["guid_to_element"]
    for el in ifc_facts["element_sample"]:
        total += 1
        if el["guid"] in guid_map:
            ok += 1
        else:
            failures.append(
                f"  GUID {el['guid']} type={el['type']} → missing IRI"
            )
    return ok, total, failures


def check_containment(
    ifc_facts: dict, graph_facts: dict
) -> tuple[int, int, list[str]]:
    g = graph_facts["graph"]
    guid_to_el = graph_facts["guid_to_element"]
    guid_to_sp = graph_facts["guid_to_spatial"]
    ok = total = 0
    failures: list[str] = []
    for rel in ifc_facts["containment_sample"]:
        total += 1
        el_iri = guid_to_el.get(rel["element_guid"])
        sp_iri = guid_to_sp.get(rel["structure_guid"])
        if el_iri is None:
            failures.append(
                f"  element {rel['element_guid']} ({rel['element_type']}) absent from graph"
            )
            continue
        if sp_iri is None:
            failures.append(
                f"  structure {rel['structure_guid']} ({rel['structure_type']}) absent from graph"
            )
            continue
        if (URIRef(sp_iri), BOT.containsElement, URIRef(el_iri)) in g:
            ok += 1
        else:
            failures.append(
                f"  missing bot:containsElement  {rel['structure_guid']} → {rel['element_guid']}"
            )
    return ok, total, failures


# ── Report rendering ──────────────────────────────────────────────────────────

C_PASS = "\033[32m✓\033[0m"
C_FAIL = "\033[31m✗\033[0m"
C_WARN = "\033[33m~\033[0m"
C_BOLD = "\033[1m"
C_RESET = "\033[0m"


def _icon(ok: bool) -> str:
    return C_PASS if ok else C_FAIL


def _count_row(label: str, ifc_n: int, graph_n: int, strict: bool = True) -> str:
    match = ifc_n == graph_n
    icon = _icon(match) if strict else (C_PASS if match else C_WARN)
    return f"  {label:<32} IFC: {ifc_n:<7} Graph: {graph_n:<7} {icon}"


def _check_row(label: str, ok: int, total: int) -> str:
    return f"  {label:<32} {ok}/{total} {_icon(ok == total)}"


# ── Main validation ───────────────────────────────────────────────────────────


def run_validation(ifc_path: Path) -> dict:
    print(f"\n{C_BOLD}{'='*64}{C_RESET}")
    print(f"{C_BOLD}  {ifc_path.name}{C_RESET}")
    size_mb = ifc_path.stat().st_size / 1024 / 1024
    print(f"  Size: {size_mb:.1f} MB")
    print(f"{'='*64}")

    rng = random.Random(RANDOM_SEED)
    all_failures: list[str] = []

    print("  [1/3] Analysing IFC with IfcOpenShell...")
    ifc_facts = collect_ifc_facts(ifc_path, rng)
    print(f"        Schema: {ifc_facts['schema']}   Elements: {ifc_facts['element_count']}")

    tmp = tempfile.NamedTemporaryFile(suffix=".ttl", delete=False)
    out_ttl = Path(tmp.name)
    tmp.close()

    try:
        print("  [2/3] Running ifc2lbd-neo conversion...")
        elapsed = run_conversion(ifc_path, out_ttl)
        ttl_mb = out_ttl.stat().st_size / 1024 / 1024
        print(f"        Done in {elapsed:.1f}s → {ttl_mb:.1f} MB TTL")

        print("  [3/3] Parsing RDF graph with rdflib...")
        graph_facts = collect_graph_facts(out_ttl)
        print(f"        bot:Element: {graph_facts['element_count']}   "
              f"lbd:PropertySet: {graph_facts['pset_count']}")
    finally:
        out_ttl.unlink(missing_ok=True)

    # ── SPATIAL STRUCTURE ─────────────────────────────────────────────────────
    print(f"\n{C_BOLD}SPATIAL STRUCTURE{C_RESET}")
    for key, label in [
        ("project", "lbd:Project"),
        ("site", "bot:Site"),
        ("building", "bot:Building"),
        ("storey", "bot:Storey"),
        ("space", "bot:Space"),
    ]:
        ifc_n = ifc_facts["spatial_counts"][key]
        graph_n = graph_facts["spatial_counts"][key]
        print(_count_row(label, ifc_n, graph_n))
        if ifc_n != graph_n:
            all_failures.append(f"Spatial {key}: IFC={ifc_n} Graph={graph_n}")

    # ── ELEMENTS ──────────────────────────────────────────────────────────────
    print(f"\n{C_BOLD}ELEMENTS{C_RESET}")
    ifc_el = ifc_facts["element_count"]
    graph_el = graph_facts["element_count"]
    print(_count_row("bot:Element (total)", ifc_el, graph_el))
    if ifc_el != graph_el:
        all_failures.append(f"bot:Element total: IFC={ifc_el} Graph={graph_el}")
        missing_types = sorted(ifc_facts["elements_by_type"].keys())
        print(f"  IFC types ({len(missing_types)}): " +
              ", ".join(missing_types[:8]) + ("..." if len(missing_types) > 8 else ""))

    # Top 5 element types for information
    top_types = sorted(ifc_facts["elements_by_type"].items(), key=lambda x: -x[1])[:5]
    for t, c in top_types:
        print(f"    {t:<35} count: {c}")

    # ── PROPERTIES ────────────────────────────────────────────────────────────
    print(f"\n{C_BOLD}PROPERTIES{C_RESET}")
    ifc_psets = ifc_facts["reachable_pset_count"]
    ifc_psets_total = ifc_facts["pset_count"]
    graph_psets = graph_facts["pset_count"]
    unreachable = ifc_psets_total - ifc_psets
    label = f"lbd:PropertySet (of {ifc_psets_total})"
    if unreachable:
        label = f"lbd:PropertySet (-{unreachable} unreachable)"
    print(_count_row(label, ifc_psets, graph_psets))
    if ifc_psets != graph_psets:
        all_failures.append(f"lbd:PropertySet: IFC reachable={ifc_psets} Graph={graph_psets}")

    ifc_sv = ifc_facts["single_value_count"]
    ifc_ev = ifc_facts["enum_value_count"]
    graph_props = graph_facts["property_count"]
    # opm:Property includes both single-value and (now multi-select) enum props
    # so graph count >= ifc_sv is expected when enum values are multi-select
    expected_min = ifc_sv
    print(_count_row("opm:Property (single-val)", ifc_sv, graph_props, strict=False))
    if graph_props < expected_min:
        all_failures.append(
            f"opm:Property count {graph_props} < expected ≥{expected_min}"
        )
    print(f"  {'IfcPropertyEnumeratedValue (IFC)':<32} count: {ifc_ev}")

    # ── RELATIONSHIPS ─────────────────────────────────────────────────────────
    print(f"\n{C_BOLD}RELATIONSHIPS{C_RESET}")
    ifc_cont = ifc_facts["containment_rel_count"]
    graph_cont = graph_facts["containment_count"]
    # Graph count ≥ IFC count is expected (one rel can have many elements)
    print(_count_row("bot:containsElement triples", ifc_cont, graph_cont, strict=False))

    # ── SPOT CHECKS ───────────────────────────────────────────────────────────
    print(f"\n{C_BOLD}SPOT CHECKS (correctness){C_RESET}")

    ok, total, fails = check_guid_iris(ifc_facts, graph_facts)
    print(_check_row("GUID → graph IRI", ok, total))
    all_failures.extend(fails)

    ok, total, fails = check_containment(ifc_facts, graph_facts)
    print(_check_row("bot:containsElement", ok, total))
    all_failures.extend(fails)

    # ── RESULT ────────────────────────────────────────────────────────────────
    overall = "PASS" if not all_failures else "FAIL"
    color = "\033[32m" if overall == "PASS" else "\033[31m"
    print(f"\n{color}{C_BOLD}RESULT: {overall}  ({len(all_failures)} failures){C_RESET}")
    for msg in all_failures[:15]:
        print(f"  {msg}")
    if len(all_failures) > 15:
        print(f"  ... and {len(all_failures) - 15} more (see JSON report)")

    return {
        "file": ifc_path.name,
        "schema": ifc_facts["schema"],
        "result": overall,
        "failures": all_failures,
        "conversion_seconds": round(elapsed, 2),
        "ttl_mb": round(ttl_mb, 1),
        "ifc": {
            "elements": ifc_el,
            "elements_by_type": ifc_facts["elements_by_type"],
            "spatial": ifc_facts["spatial_counts"],
            "psets_total": ifc_psets_total,
            "psets_reachable": ifc_psets,
            "single_values": ifc_sv,
            "enum_values": ifc_ev,
        },
        "graph": {
            "elements": graph_el,
            "spatial": graph_facts["spatial_counts"],
            "psets": graph_psets,
            "properties": graph_props,
            "containment_triples": graph_cont,
        },
    }


# ── Entry point ───────────────────────────────────────────────────────────────


def main() -> None:
    if not BIN.exists():
        print(f"ERROR: CLI binary not found at {BIN}", file=sys.stderr)
        print("       Build it with: cargo build --release -p ifc2lbd-cli", file=sys.stderr)
        sys.exit(1)

    if len(sys.argv) > 1:
        ifc_files = [Path(p).resolve() for p in sys.argv[1:]]
    else:
        ifc_files = sorted(
            p for p in ROOT.glob("*.ifc")
            if p.stat().st_size <= MAX_FILE_BYTES
        )
        if not ifc_files:
            print("No IFC files ≤15 MB found in project root. Pass paths as arguments.")
            sys.exit(1)
        print(f"Found {len(ifc_files)} IFC file(s) ≤15 MB: {[p.name for p in ifc_files]}")

    reports = []
    for ifc_path in ifc_files:
        if not ifc_path.exists():
            print(f"ERROR: file not found: {ifc_path}", file=sys.stderr)
            continue
        try:
            reports.append(run_validation(ifc_path))
        except Exception as exc:
            print(f"\nERROR validating {ifc_path.name}: {exc}", file=sys.stderr)
            import traceback; traceback.print_exc()
            reports.append({
                "file": ifc_path.name,
                "result": "ERROR",
                "error": str(exc),
            })

    out_dir = ROOT / "test-results"
    out_dir.mkdir(exist_ok=True)
    out_file = out_dir / "validation_report.json"
    out_file.write_text(json.dumps(reports, indent=2))
    print(f"\nFull report written to {out_file.relative_to(ROOT)}")

    n_fail = sum(1 for r in reports if r.get("result") != "PASS")
    n_pass = sum(1 for r in reports if r.get("result") == "PASS")
    print(f"Summary: {n_pass} passed, {n_fail} failed out of {len(reports)} file(s)\n")
    sys.exit(1 if n_fail > 0 else 0)


if __name__ == "__main__":
    main()
