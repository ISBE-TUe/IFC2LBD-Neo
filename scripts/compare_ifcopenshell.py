#!/usr/bin/env python3
"""Cross-check qto-geometry against IfcOpenShell on the same models.

The authored quantities in an IFC file are a useful oracle but not an
infallible one: exporters disagree, some values are stale relative to the
geometry, and some fields are used for something other than their definition.
When our value differs from the authored one, that alone cannot say which is
wrong.

IfcOpenShell is an independent implementation of the same problem, built on
OCCT through IfcGeom. Comparing against it separates the two cases:

  * we differ from authored AND from IfcOpenShell  -> suspect our maths
  * we differ from authored but AGREE with IfcOpenShell -> the authored value
    is the outlier
  * we differ from IfcOpenShell in a direction explained by its tessellation
    -> we are the more exact of the two

That last case is real. IfcOpenShell tessellates curved surfaces, so an
inscribed polygon under-reports a circular section by around 1%, while the
analytic path computes pi*(R^2-r^2) exactly.

Usage:
    python3 -m venv venv && venv/bin/pip install ifcopenshell
    cargo build --release -p qto-validate
    ./target/release/qto-validate <model.ifc> --json ours.json
    venv/bin/python scripts/compare_ifcopenshell.py <model.ifc> ours.json [limit]
"""
import collections
import json
import statistics as st
import sys
import time

import ifcopenshell
import ifcopenshell.geom
import ifcopenshell.util.shape as shape_utils


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    path, ours_json = sys.argv[1], sys.argv[2]
    limit = int(sys.argv[3]) if len(sys.argv) > 3 else 1_000_000

    model = ifcopenshell.open(path)
    basename = path.split("/")[-1]

    ours = {
        r["guid"]: (r["value"], r["authored"])
        for r in json.load(open(ours_json))
        if r.get("outcome") == "computed"
        and r["quantity"] == "NetVolume"
        and r["file"].endswith(basename)
    }
    if not ours:
        print(f"no computed NetVolume for {basename} in {ours_json}")
        return 1

    print(f"{basename}: schema {model.schema}, {len(ours)} elements to compare")

    settings = ifcopenshell.geom.settings()
    iterator = ifcopenshell.geom.iterator(settings, model, 4)
    rows, started = [], time.time()
    if iterator.initialize():
        while len(rows) < limit:
            sh = iterator.get()
            if sh.guid in ours:
                try:
                    ios_volume = shape_utils.get_volume(sh.geometry)
                except Exception:
                    ios_volume = None
                if ios_volume:
                    our_volume, authored = ours[sh.guid]
                    rows.append((sh.guid, authored, our_volume, ios_volume))
            if not iterator.next():
                break
    print(f"  IfcOpenShell measured {len(rows)} in {time.time() - started:.1f}s")
    if not rows:
        return 1

    def relative(a, b):
        return abs(a - b) / abs(b) if abs(b) > 1e-12 else float("inf")

    def summarise(label, errors):
        within = sum(1 for e in errors if e <= 0.001)
        print(
            f"  {label:<28} median {st.median(errors) * 100:8.3f}%"
            f"   within 0.1%: {within}/{len(errors)} ({100 * within / len(errors):.1f}%)"
        )

    print("\n  NetVolume agreement")
    summarise("IfcOpenShell vs authored", [relative(r[3], r[1]) for r in rows])
    summarise("ours vs authored", [relative(r[2], r[1]) for r in rows])
    summarise("ours vs IfcOpenShell", [relative(r[3], r[2]) for r in rows])

    disagreements = [r for r in rows if relative(r[2], r[3]) > 0.001]
    if disagreements:
        print(f"\n  where we and IfcOpenShell differ ({len(disagreements)}):")
        print(f"    median ours/ifcopenshell {st.median([d[2] / d[3] for d in disagreements]):.6f}")
        print(f"    median ours/authored     {st.median([d[2] / d[1] for d in disagreements if d[1]]):.6f}")
        print(f"    median ios/authored      {st.median([d[3] / d[1] for d in disagreements if d[1]]):.6f}")
        kinds = collections.Counter(model.by_guid(d[0]).is_a() for d in disagreements)
        for kind, n in kinds.most_common(5):
            print(f"      {kind:<24} n={n}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
