#!/usr/bin/env python3
# On macOS with Homebrew: use `/opt/homebrew/bin/python3` if ifcopenshell/rdflib
# are not on the system python. Example: python3.14 scripts/validate_qto.py model.ifc
"""
Comprehensive QTO validation: compare ifc2lbd-neo quantity takeoff against IfcOpenShell.

IfcOpenShell tessellates the full geometry including Boolean operations (CSG),
so its net volumes are post-opening-subtraction — the ground truth for NetVolume.
All other quantities are validated against IFC BaseQuantities from the exporter.

Usage:
    python3 scripts/validate_qto.py model.ifc [model2.ifc ...]
    python3 scripts/validate_qto.py --threshold 0.02   # 2% tolerance
    python3 scripts/validate_qto.py --ifc-only model.ifc   # skip CLI, dump IfcOpenShell values

Requirements:
    pip install ifcopenshell rdflib
    cargo build --release -p ifc2lbd-cli
"""

import argparse
import json
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import ifcopenshell
import ifcopenshell.geom
import ifcopenshell.util.unit
import ifcopenshell.util.element
import rdflib
from rdflib import Graph

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target" / "release" / "ifc2lbd-neo"
BASE_URI = "https://validate.test/"
DEFAULT_THRESHOLD = 0.05

# Element types that we validate QTO for
ELEMENT_TYPES_WITH_VOLUME = [
    "IfcWall", "IfcWallStandardCase", "IfcSlab", "IfcBeam", "IfcColumn",
    "IfcMember", "IfcPile", "IfcFooting", "IfcRoof", "IfcStairFlight",
    "IfcRampFlight", "IfcPlate", "IfcCovering", "IfcBuildingElementProxy",
    "IfcDoor", "IfcWindow",
]

# CLI modules needed to emit quantity triples
QTO_CLI_MODULES = [
    "neo-cleanup-preprocess",
    "neo-qto-preprocess",
    "neo-bot-producer",
    "neo-beo-producer",
    "neo-bsdd-match-preprocess",
    "neo-bsdd-producer",
    "neo-turtle-serializer",
    "neo-file-export",
]

# Maps IfcType → IRI prefix used by our pipeline
IFC_TYPE_TO_IRI_PREFIX: Dict[str, str] = {
    "IfcWall": "wall",
    "IfcWallStandardCase": "wall",
    "IfcSlab": "slab",
    "IfcBeam": "beam",
    "IfcColumn": "column",
    "IfcMember": "member",
    "IfcPlate": "plate",
    "IfcCovering": "covering",
    "IfcFooting": "footing",
    "IfcPile": "pile",
    "IfcRoof": "roof",
    "IfcStairFlight": "stairflight",
    "IfcRampFlight": "rampflight",
    "IfcBuildingElementProxy": "buildingelement",
    "IfcDoor": "door",
    "IfcWindow": "window",
}

# Quantities classified by dimension (determines scaling)
LENGTH_QUANTITIES = {"Length", "Height", "Width", "Depth", "Perimeter"}
AREA_QUANTITIES = {
    "GrossArea", "NetArea", "GrossFloorArea", "NetFloorArea",
    "GrossFootprintArea", "NetFootprintArea", "GrossSideArea",
    "GrossWallArea", "NetWallArea",
}
VOLUME_QUANTITIES = {"NetVolume", "GrossVolume"}

# All quantity names we track
ALL_SCALAR_QUANTITIES = LENGTH_QUANTITIES | AREA_QUANTITIES | VOLUME_QUANTITIES

C_PASS = "\033[32m✓\033[0m"
C_FAIL = "\033[31m✗\033[0m"
C_WARN = "\033[33m~\033[0m"
C_BOLD = "\033[1m"
C_RESET = "\033[0m"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _normalize_guid(guid: str) -> str:
    """Our pipeline replaces '$' with '_' when building IRIs; normalize to match."""
    return guid.replace("$", "_")


def _mesh_volume(verts, faces) -> float:
    """Signed volume via divergence theorem on a closed triangle mesh."""
    try:
        import numpy as np
        v = np.array(verts).reshape(-1, 3)
        f = np.array(faces, dtype=int).reshape(-1, 3)
        v0, v1, v2 = v[f[:, 0]], v[f[:, 1]], v[f[:, 2]]
        return abs(float(np.sum(np.einsum("ij,ij->i", v0, np.cross(v1, v2))) / 6.0))
    except ImportError:
        pts = list(zip(verts[0::3], verts[1::3], verts[2::3]))
        total = 0.0
        for i in range(0, len(faces), 3):
            a, b, c = faces[i], faces[i + 1], faces[i + 2]
            v0, v1, v2 = pts[a], pts[b], pts[c]
            cx = v1[1] * v2[2] - v1[2] * v2[1]
            cy = v1[2] * v2[0] - v1[0] * v2[2]
            cz = v1[0] * v2[1] - v1[1] * v2[0]
            total += v0[0] * cx + v0[1] * cy + v0[2] * cz
        return abs(total / 6.0)


def _scale_for_quantity(qty_name: str, length_scale: float) -> float:
    """Return the multiplicative scale factor for converting raw IFC units to SI."""
    if qty_name in LENGTH_QUANTITIES:
        return length_scale
    if qty_name in AREA_QUANTITIES:
        return length_scale ** 2
    if qty_name in VOLUME_QUANTITIES:
        return length_scale ** 3
    # Unknown quantity: assume dimensionless
    return 1.0


# ---------------------------------------------------------------------------
# IfcOpenShell: geometry + BaseQuantities
# ---------------------------------------------------------------------------

def compute_ifc_data(ifc_path: Path) -> Dict[str, dict]:
    """
    Return a dict keyed by normalised GlobalId:
    {
        "type":             str,
        "name":             str,
        "ifc_csg_net_volume": float | None,   # CSG tessellated volume (ground truth)
        "ifc_base": {                          # from IFC BaseQuantities
            "NetVolume":  float,
            "GrossVolume": float,
            "Length": float,
            ...
        },
        "error": str | None,
    }
    """
    model = ifcopenshell.open(str(ifc_path))
    length_scale = ifcopenshell.util.unit.calculate_unit_scale(model)

    settings = ifcopenshell.geom.settings()
    settings.set(settings.USE_WORLD_COORDS, False)

    results: Dict[str, dict] = {}

    for ifc_type in ELEMENT_TYPES_WITH_VOLUME:
        for el in model.by_type(ifc_type):
            guid = _normalize_guid(el.GlobalId)
            entry: dict = {
                "type": el.is_a(),
                "name": el.Name or "",
                "ifc_csg_net_volume": None,
                "ifc_base": {},
                "error": None,
            }

            # CSG-evaluated geometry volume (ground truth for NetVolume)
            try:
                shape = ifcopenshell.geom.create_shape(settings, el)
                raw = _mesh_volume(shape.geometry.verts, shape.geometry.faces)
                entry["ifc_csg_net_volume"] = round(raw * (length_scale ** 3), 8)
            except Exception as exc:
                entry["error"] = str(exc)

            # IFC exporter BaseQuantities (reference for all scalar quantities)
            for _qset_name, quantities in ifcopenshell.util.element.get_psets(
                el, qtos_only=True
            ).items():
                for qty_name, raw_val in quantities.items():
                    if qty_name not in ALL_SCALAR_QUANTITIES:
                        continue
                    if raw_val is None:
                        continue
                    try:
                        scaled = float(raw_val) * _scale_for_quantity(qty_name, length_scale)
                    except (TypeError, ValueError):
                        continue
                    # First occurrence wins (multiple QTO sets are rare but possible)
                    if qty_name not in entry["ifc_base"]:
                        entry["ifc_base"][qty_name] = round(scaled, 8)

            results[guid] = entry

    return results


# ---------------------------------------------------------------------------
# CLI conversion
# ---------------------------------------------------------------------------

def run_conversion(ifc_path: Path, out_ttl: Path) -> float:
    args = [str(BIN), str(ifc_path), "-o", str(out_ttl), "-u", BASE_URI]
    for mod in QTO_CLI_MODULES:
        args += ["--module", mod]
    t0 = time.monotonic()
    result = subprocess.run(args, capture_output=True, text=True, timeout=600)
    elapsed = time.monotonic() - t0
    if result.returncode != 0:
        raise RuntimeError(f"CLI exited {result.returncode}:\n{result.stderr[:3000]}")
    return elapsed


# ---------------------------------------------------------------------------
# RDF quantity extraction
# ---------------------------------------------------------------------------

_SPARQL = """
PREFIX bsddm:  <https://w3id.org/ifc2lbd/bsdd-meta#>
PREFIX opm:    <https://w3id.org/opm#>
PREFIX rdfs:   <http://www.w3.org/2000/01/rdf-schema#>
PREFIX schema: <http://schema.org/>

SELECT ?element ?propLabel ?value WHERE {
  ?element bsddm:hasQuantitySet ?qset .
  ?qset    bsddm:hasProperty    ?prop .
  ?prop    rdfs:label           ?propLabel .
  ?prop    opm:hasPropertyState ?state .
  ?state   schema:value         ?value .
}
"""


def _parse_iri(iri: str) -> Tuple[str, str]:
    """
    Parse an element IRI into (type_prefix, guid).

    IRI tail format: <type_prefix>_<22-char-guid>
    e.g. "wall_3LF03GdX..." → ("wall", "3LF03GdX...")
         "member_3LF03GdX..." → ("member", "3LF03GdX...")
    """
    tail = iri.rsplit("/", 1)[-1]
    if len(tail) > 23 and tail[-23] == "_":
        guid = tail[-22:]
        type_prefix = tail[:-23]
    elif len(tail) >= 22:
        guid = tail[-22:]
        type_prefix = tail[:-22].rstrip("_")
    else:
        guid = tail
        type_prefix = ""
    return type_prefix, guid


def extract_quantities_from_ttl(ttl_path: Path) -> dict:
    """
    Returns a structure:
    {
        "_by_prefix": {
            (guid, type_prefix): {qty_name: float, ...},
            ...
        },
        guid: {qty_name: float, ...},   # merged fallback (last-write wins)
        ...
    }

    Label normalisation:
        "Qto_WallBaseQuantities:GrossVolume" → "GrossVolume"
        "GrossVolume"                        → "GrossVolume"
        "grossVolume"                        → "GrossVolume"
    """
    g = Graph()
    g.parse(str(ttl_path), format="turtle")

    # Collect per-IRI quantities
    iri_qtys: Dict[str, dict] = {}
    for row in g.query(_SPARQL):
        iri = str(row.element)
        label = str(row.propLabel)

        # Normalise label: take everything after the last colon, then PascalCase
        local = label.split(":")[-1].strip() if ":" in label else label.strip()
        # Ensure first letter is upper-case (handles camelCase inputs)
        qty_name = local[0].upper() + local[1:] if local else local

        try:
            val = float(str(row.value))
        except (ValueError, TypeError):
            continue

        iri_qtys.setdefault(iri, {})[qty_name] = val

    # Build lookup structures
    by_prefix: Dict[Tuple[str, str], dict] = {}
    by_guid: Dict[str, dict] = {}

    for iri, qtys in iri_qtys.items():
        type_prefix, guid = _parse_iri(iri)
        by_prefix[(guid, type_prefix)] = qtys
        # Merged fallback: all quantities for this guid regardless of type prefix
        by_guid.setdefault(guid, {}).update(qtys)

    result = dict(by_guid)
    result["_by_prefix"] = by_prefix  # type: ignore[assignment]
    return result


def lookup_our_qty(
    guid: str,
    ifc_type: str,
    our_quantities: dict,
    qty_name: str,
) -> Optional[float]:
    """
    Look up a quantity for a given element guid, preferring the IRI whose
    type_prefix matches the expected IFC type (resolves plate/member collisions).
    Falls back to the merged-guid dict if no type-match is found.
    """
    by_prefix = our_quantities.get("_by_prefix", {})
    expected_prefix = IFC_TYPE_TO_IRI_PREFIX.get(ifc_type, "")
    if expected_prefix:
        val = by_prefix.get((guid, expected_prefix), {}).get(qty_name)
        if val is not None:
            return val
    # Fallback: any IRI with this guid
    return our_quantities.get(guid, {}).get(qty_name)


# ---------------------------------------------------------------------------
# Comparison data structures
# ---------------------------------------------------------------------------

@dataclass
class VolumeResult:
    guid: str
    ifc_type: str
    name: str
    ifc_csg_vol: Optional[float]
    our_vol: Optional[float]
    ifc_base_vol: Optional[float]
    diff_pct: Optional[float]
    status: str  # "ok" | "diff" | "missing" | "no_geometry"


@dataclass
class ScalarResult:
    guid: str
    ifc_type: str
    qty_name: str
    ifc_base_val: float
    our_val: Optional[float]
    diff_pct: Optional[float]
    status: str  # "ok" | "diff" | "missing"


@dataclass
class ComparisonReport:
    volume_results: List[VolumeResult] = field(default_factory=list)
    scalar_results: List[ScalarResult] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Comparison logic
# ---------------------------------------------------------------------------

def compare(
    ifc_data: Dict[str, dict],
    our_quantities: dict,
    threshold: float,
) -> ComparisonReport:
    report = ComparisonReport()

    for guid, entry in ifc_data.items():
        ifc_type = entry["type"]
        ifc_csg_vol = entry["ifc_csg_net_volume"]
        ifc_base = entry["ifc_base"]

        # --- Volume check (CSG ground truth) ---
        our_vol = lookup_our_qty(guid, ifc_type, our_quantities, "NetVolume")
        ifc_base_vol = ifc_base.get("NetVolume")

        diff_pct: Optional[float] = None
        if ifc_csg_vol is None:
            vol_status = "no_geometry"
        elif entry["error"] and ifc_csg_vol is None:
            vol_status = "no_geometry"
        elif our_vol is None:
            vol_status = "missing"
        else:
            diff_pct = abs(ifc_csg_vol - our_vol) / max(abs(ifc_csg_vol), 1e-9)
            vol_status = "ok" if diff_pct <= threshold else "diff"

        report.volume_results.append(VolumeResult(
            guid=guid,
            ifc_type=ifc_type,
            name=entry["name"],
            ifc_csg_vol=ifc_csg_vol,
            our_vol=our_vol,
            ifc_base_vol=ifc_base_vol,
            diff_pct=diff_pct,
            status=vol_status,
        ))

        # --- All other scalar quantities (vs IFC BaseQuantities) ---
        for qty_name, base_val in ifc_base.items():
            if base_val is None or base_val <= 0:
                continue
            our_val = lookup_our_qty(guid, ifc_type, our_quantities, qty_name)

            if our_val is None:
                scalar_status = "missing"
                scalar_diff_pct = None
            else:
                scalar_diff_pct = abs(base_val - our_val) / max(abs(base_val), 1e-9)
                scalar_status = "ok" if scalar_diff_pct <= threshold else "diff"

            report.scalar_results.append(ScalarResult(
                guid=guid,
                ifc_type=ifc_type,
                qty_name=qty_name,
                ifc_base_val=base_val,
                our_val=our_val,
                diff_pct=scalar_diff_pct,
                status=scalar_status,
            ))

    return report


# ---------------------------------------------------------------------------
# Report formatting
# ---------------------------------------------------------------------------

def _pct_str(v: Optional[float]) -> str:
    return f"{v * 100:.0f}%" if v is not None else "N/A"


def print_report(
    ifc_path: Path,
    ifc_data: Dict[str, dict],
    our_count: int,
    elapsed: float,
    ttl_mb: float,
    report: ComparisonReport,
    threshold: float,
) -> dict:
    print(f"\n{C_BOLD}{'=' * 64}{C_RESET}")
    print(f"{C_BOLD}  QTO: {ifc_path.name}{C_RESET}")
    print(f"{'=' * 64}")

    n_elements = len(ifc_data)
    n_tessellated = sum(1 for e in ifc_data.values() if e["ifc_csg_net_volume"] is not None)
    n_base = sum(1 for e in ifc_data.values() if e["ifc_base"])

    print(f"  [1/3] IfcOpenShell: {n_elements} elements | "
          f"{n_tessellated} tessellated | {n_base} have BaseQuantities")
    print(f"  [2/3] Running ifc2lbd-neo...  {elapsed:.1f}s -> {ttl_mb:.0f}MB TTL")
    print(f"  [3/3] Extracting quantities...  {our_count} elements in graph")

    # --- Volume section ---
    vol_ok      = [r for r in report.volume_results if r.status == "ok"]
    vol_diff    = [r for r in report.volume_results if r.status == "diff"]
    vol_missing = [r for r in report.volume_results if r.status == "missing"]
    vol_no_geom = [r for r in report.volume_results if r.status == "no_geometry"]

    # Build missing-by-type summary
    missing_by_type: Dict[str, int] = defaultdict(int)
    for r in vol_missing:
        missing_by_type[r.ifc_type] += 1
    missing_type_str = "{" + ", ".join(
        f"{t}:{c}" for t, c in sorted(missing_by_type.items())
    ) + "}" if missing_by_type else ""

    # Top diff elements (sorted by diff_pct descending)
    top_diffs = sorted(vol_diff, key=lambda r: -(r.diff_pct or 0))[:5]
    top_diff_str = ""
    if top_diffs:
        snippets = []
        for r in top_diffs:
            pct = f"{r.diff_pct * 100:.0f}%" if r.diff_pct is not None else "N/A"
            snippets.append(
                f"{r.ifc_type} {r.guid[:8]} {pct}: "
                f"{r.ifc_csg_vol:.4f}->{r.our_vol:.4f}"
            )
        top_diff_str = "  top: [" + ", ".join(snippets) + "]"

    print(f"\n{C_BOLD}VOLUME (NetVolume - CSG ground truth):{C_RESET}")
    print(f"  Pass (<=5%):  {len(vol_ok):5d}  {C_PASS}")
    print(f"  Diff  (>5%):  {len(vol_diff):5d}  {C_FAIL}"
          + (f"    {top_diff_str}" if top_diff_str else ""))
    missing_line = f"  Missing:      {len(vol_missing):5d}  {C_WARN}"
    if missing_type_str:
        missing_line += f"    by type: {missing_type_str}"
    print(missing_line)

    # --- Scalar quantities section ---
    # Group scalar results by quantity name
    scalar_by_qty: Dict[str, List[ScalarResult]] = defaultdict(list)
    for r in report.scalar_results:
        scalar_by_qty[r.qty_name].append(r)

    if scalar_by_qty:
        print(f"\n{C_BOLD}SCALAR QUANTITIES (vs IFC BaseQuantities):{C_RESET}")
        header = f"  {'Quantity':<20}  {'Pass':>6}  {'Diff':>6}  {'Missing':>8}"
        print(header)
        print("  " + "-" * (len(header) - 2))

        qty_order = sorted(
            scalar_by_qty.keys(),
            key=lambda q: (q not in VOLUME_QUANTITIES, q not in AREA_QUANTITIES, q),
        )
        for qty_name in qty_order:
            results = scalar_by_qty[qty_name]
            n_ok   = sum(1 for r in results if r.status == "ok")
            n_diff = sum(1 for r in results if r.status == "diff")
            n_miss = sum(1 for r in results if r.status == "missing")
            diff_marker = f"  {C_FAIL}" if n_diff else ""
            miss_marker = f"  {C_WARN}" if n_miss else ""
            print(f"  {qty_name:<20}  {n_ok:>6}  {n_diff:>6}{diff_marker}  {n_miss:>8}{miss_marker}")

    # --- Overall result ---
    n_vol_fail = len(vol_diff)
    n_vol_miss = len(vol_missing)
    n_scalar_diff = sum(1 for r in report.scalar_results if r.status == "diff")

    overall_fail = n_vol_fail > 0 or n_vol_miss > 0 or n_scalar_diff > 0
    overall = "FAIL" if overall_fail else "PASS"
    color = "\033[31m" if overall_fail else "\033[32m"
    print(
        f"\n{color}{C_BOLD}RESULT: {overall}  "
        f"({n_vol_fail} volume diffs, {n_vol_miss} missing volumes, "
        f"{n_scalar_diff} scalar mismatches){C_RESET}"
    )

    # --- JSON-serialisable report ---
    return {
        "file": ifc_path.name,
        "result": overall,
        "threshold_pct": threshold * 100,
        "summary": {
            "elements": n_elements,
            "tessellated": n_tessellated,
            "have_base_quantities": n_base,
            "graph_elements": our_count,
        },
        "volume": {
            "pass": len(vol_ok),
            "diff": n_vol_fail,
            "missing": n_vol_miss,
            "no_geometry": len(vol_no_geom),
            "top_diffs": [
                {
                    "guid": r.guid,
                    "type": r.ifc_type,
                    "name": r.name,
                    "ifc_csg_net_volume_m3": r.ifc_csg_vol,
                    "our_net_volume_m3": r.our_vol,
                    "ifc_base_net_volume_m3": r.ifc_base_vol,
                    "diff_pct": round(r.diff_pct * 100, 2) if r.diff_pct is not None else None,
                    "status": r.status,
                }
                for r in sorted(
                    vol_diff + vol_missing,
                    key=lambda r: -(r.diff_pct or 0),
                )
            ],
        },
        "scalars": {
            qty_name: {
                "pass":    sum(1 for r in results if r.status == "ok"),
                "diff":    sum(1 for r in results if r.status == "diff"),
                "missing": sum(1 for r in results if r.status == "missing"),
                "diffs": [
                    {
                        "guid": r.guid,
                        "type": r.ifc_type,
                        "ifc_base_val": r.ifc_base_val,
                        "our_val": r.our_val,
                        "diff_pct": round(r.diff_pct * 100, 2) if r.diff_pct is not None else None,
                    }
                    for r in results
                    if r.status == "diff"
                ],
            }
            for qty_name, results in scalar_by_qty.items()
        },
    }


# ---------------------------------------------------------------------------
# Main validation entry point
# ---------------------------------------------------------------------------

def run_validation(ifc_path: Path, threshold: float, ifc_only: bool) -> dict:
    print(f"\n{C_BOLD}{'=' * 64}{C_RESET}")
    print(f"{C_BOLD}  QTO: {ifc_path.name}{C_RESET}")
    print(f"{'=' * 64}")

    print("  [1/3] Computing quantities with IfcOpenShell (CSG + BaseQuantities)...")
    ifc_data = compute_ifc_data(ifc_path)
    n_tessellated = sum(1 for e in ifc_data.values() if e["ifc_csg_net_volume"] is not None)
    n_base = sum(1 for e in ifc_data.values() if e["ifc_base"])
    print(f"        {len(ifc_data)} elements | {n_tessellated} tessellated | "
          f"{n_base} have BaseQuantities")

    if ifc_only:
        print(f"\n  --ifc-only: skipping CLI conversion.")
        return {
            "file": ifc_path.name,
            "result": "IFC_ONLY",
            "ifc_data": {
                guid: {
                    "type": e["type"],
                    "name": e["name"],
                    "ifc_csg_net_volume_m3": e["ifc_csg_net_volume"],
                    "ifc_base": e["ifc_base"],
                }
                for guid, e in ifc_data.items()
                if e["ifc_csg_net_volume"] is not None or e["ifc_base"]
            },
        }

    tmp = tempfile.NamedTemporaryFile(suffix=".ttl", delete=False)
    out_ttl = Path(tmp.name)
    tmp.close()
    try:
        print("  [2/3] Running ifc2lbd-neo...")
        elapsed = run_conversion(ifc_path, out_ttl)
        ttl_mb = out_ttl.stat().st_size / 1024 / 1024
        print(f"        {elapsed:.1f}s -> {ttl_mb:.0f}MB TTL")

        print("  [3/3] Extracting quantities from RDF output...")
        our_quantities = extract_quantities_from_ttl(out_ttl)
        # Count unique guids (exclude the _by_prefix meta key)
        our_count = sum(1 for k in our_quantities if k != "_by_prefix")
        print(f"        {our_count} elements with quantity sets in graph")
    finally:
        out_ttl.unlink(missing_ok=True)

    comparison = compare(ifc_data, our_quantities, threshold)
    return print_report(
        ifc_path, ifc_data, our_count, elapsed, ttl_mb, comparison, threshold
    )


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Validate ifc2lbd-neo QTO output against IfcOpenShell geometry"
    )
    parser.add_argument("ifc_files", nargs="*", help="IFC files to validate")
    parser.add_argument(
        "--threshold", "-t", type=float, default=DEFAULT_THRESHOLD,
        help=f"Acceptable relative diff (default {DEFAULT_THRESHOLD} = 5%%)",
    )
    parser.add_argument(
        "--ifc-only", action="store_true",
        help="Only run IfcOpenShell, skip CLI conversion (dump ground truth)",
    )
    args = parser.parse_args()

    if not args.ifc_only and not BIN.exists():
        print(f"ERROR: CLI binary not found at {BIN}", file=sys.stderr)
        print("       Build it with: cargo build --release -p ifc2lbd-cli", file=sys.stderr)
        sys.exit(1)

    if args.ifc_files:
        ifc_paths = [Path(p).resolve() for p in args.ifc_files]
    else:
        ifc_paths = sorted(ROOT.glob("*.ifc"))
        if not ifc_paths:
            print("No IFC files found in project root. Pass paths as arguments.")
            sys.exit(1)

    reports = []
    for ifc_path in ifc_paths:
        if not ifc_path.exists():
            print(f"ERROR: not found: {ifc_path}", file=sys.stderr)
            continue
        try:
            reports.append(run_validation(ifc_path, args.threshold, args.ifc_only))
        except Exception as exc:
            print(f"\nERROR {ifc_path.name}: {exc}", file=sys.stderr)
            import traceback
            traceback.print_exc()
            reports.append({"file": ifc_path.name, "result": "ERROR", "error": str(exc)})

    out_dir = ROOT / "test-results"
    out_dir.mkdir(exist_ok=True)
    out_file = out_dir / "qto_validation_report.json"
    out_file.write_text(json.dumps(reports, indent=2))
    print(f"\nFull report -> {out_file.relative_to(ROOT)}")

    n_fail = sum(1 for r in reports if r.get("result") not in ("PASS", "IFC_ONLY"))
    n_pass = sum(1 for r in reports if r.get("result") == "PASS")
    print(f"Summary: {n_pass} passed, {n_fail} failed / {len(reports)} file(s)\n")
    sys.exit(1 if n_fail > 0 else 0)


if __name__ == "__main__":
    main()
