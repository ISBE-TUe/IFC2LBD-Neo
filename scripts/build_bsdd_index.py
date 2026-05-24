#!/usr/bin/env python3
"""
Build a compact bSDD IFC 4.3 index from a full export JSON.

Input:  ifc-4.3.json (full dump)
Output: crates/lbd-converter/resources/bsdd_ifc4x3_index.json
"""

from __future__ import annotations

import argparse
import gzip
import json
from pathlib import Path


def normalize(value: str) -> str:
    return "".join(ch.lower() for ch in value if ch.isalnum())


def dedupe_sorted(values: list[str]) -> list[str]:
    return sorted(set(values))


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--input", default="ifc-4.3.json")
    p.add_argument("--output", default="crates/lbd-converter/resources/bsdd_ifc4x3_index.json.gz")
    args = p.parse_args()

    src = Path(args.input)
    dst = Path(args.output)

    data = json.loads(src.read_text(encoding="utf-8"))
    classes = data.get("Classes", [])
    properties = data.get("Properties", [])

    class_code_by_norm: dict[str, str] = {}
    class_meta_by_code_norm: dict[str, dict[str, str]] = {}
    prop_name_by_code_norm: dict[str, str] = {}
    prop_meta_by_code_norm: dict[str, dict[str, str]] = {}
    exact: dict[str, str] = {}
    exact_meta: dict[str, dict[str, str]] = {}
    by_pset_prop: dict[str, list[str]] = {}
    by_class_prop: dict[str, list[str]] = {}
    by_prop: dict[str, list[str]] = {}

    for prop in properties:
        code = str(prop.get("Code", "")).strip()
        if not code:
            continue
        prop_name_by_code_norm[normalize(code)] = str(prop.get("Name", "")).strip()
        prop_meta_by_code_norm[normalize(code)] = {
            "name": str(prop.get("Name", "")).strip(),
            "description": str(prop.get("Description", "")).strip(),
            "definition": str(prop.get("Definition", "")).strip(),
            "value_kind": str(prop.get("PropertyValueKind", "")).strip(),
        }

    for cls in classes:
        class_code = str(cls.get("Code", "")).strip()
        if not class_code:
            continue
        class_norm = normalize(class_code)
        class_code_by_norm[class_norm] = class_code
        class_meta_by_code_norm[class_norm] = {
            "name": str(cls.get("Name", "")).strip(),
            "definition": str(cls.get("Definition", "")).strip(),
        }

        for cp in cls.get("ClassProperties") or []:
            prop_code = str(cp.get("PropertyCode", "")).strip()
            if not prop_code:
                continue
            pset = str(cp.get("PropertySet", "")).strip()

            prop_norm = normalize(prop_code)
            pset_norm = normalize(pset)

            exact[f"{class_norm}|{pset_norm}|{prop_norm}"] = prop_code
            exact_meta[f"{class_norm}|{pset_norm}|{prop_norm}"] = {
                "property_set": pset,
                "class_property_code": str(cp.get("Code", "")).strip(),
            }
            by_pset_prop.setdefault(f"{pset_norm}|{prop_norm}", []).append(prop_code)
            by_class_prop.setdefault(f"{class_norm}|{prop_norm}", []).append(prop_code)
            by_prop.setdefault(prop_norm, []).append(prop_code)

    for k in list(by_pset_prop.keys()):
        by_pset_prop[k] = dedupe_sorted(by_pset_prop[k])
    for k in list(by_class_prop.keys()):
        by_class_prop[k] = dedupe_sorted(by_class_prop[k])
    for k in list(by_prop.keys()):
        by_prop[k] = dedupe_sorted(by_prop[k])

    index = {
        "format": "ifc2lbd-bsdd-index-v1",
        "dictionary_code": data.get("DictionaryCode"),
        "dictionary_version": data.get("DictionaryVersion"),
        "organization_code": data.get("OrganizationCode"),
        "class_code_by_norm": class_code_by_norm,
        "class_meta_by_code_norm": class_meta_by_code_norm,
        "prop_name_by_code_norm": prop_name_by_code_norm,
        "prop_meta_by_code_norm": prop_meta_by_code_norm,
        "exact": exact,
        "exact_meta": exact_meta,
        "by_pset_prop": by_pset_prop,
        "by_class_prop": by_class_prop,
        "by_prop": by_prop,
    }

    dst.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(index, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    with gzip.open(dst, "wb", compresslevel=9) as fh:
        fh.write(payload)
    print(f"wrote {dst} ({dst.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
