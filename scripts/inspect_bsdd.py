#!/usr/bin/env python3
"""
Lightweight inspector for large bSDD JSON exports.

Usage examples:
  scripts/inspect_bsdd.py summary
  scripts/inspect_bsdd.py class IfcDoor
  scripts/inspect_bsdd.py property OverallHeightIfcDoor
  scripts/inspect_bsdd.py find-class door --limit 10
  scripts/inspect_bsdd.py find-property height --limit 10
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


DEFAULT_JSON = "ifc-4.3.json"


def load_dataset(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as fh:
        return json.load(fh)


def cmd_summary(data: dict) -> None:
    print(f"DictionaryCode: {data.get('DictionaryCode')}")
    print(f"DictionaryVersion: {data.get('DictionaryVersion')}")
    print(f"OrganizationCode: {data.get('OrganizationCode')}")
    print(f"LanguageIsoCode: {data.get('LanguageIsoCode')}")
    print(f"Classes: {len(data.get('Classes', []))}")
    print(f"Properties: {len(data.get('Properties', []))}")


def cmd_class(data: dict, code: str, limit_props: int) -> int:
    for cls in data.get("Classes", []):
        if str(cls.get("Code", "")).lower() == code.lower():
            cps = cls.get("ClassProperties") or []
            print(f"Code: {cls.get('Code')}")
            print(f"Name: {cls.get('Name')}")
            print(f"ClassType: {cls.get('ClassType')}")
            print(f"ParentClassCode: {cls.get('ParentClassCode')}")
            print(f"ClassProperties: {len(cps)}")
            if cps:
                print("ClassProperties sample:")
                for cp in cps[:limit_props]:
                    print(
                        f"- PropertyCode={cp.get('PropertyCode')} "
                        f"PropertySet={cp.get('PropertySet')} "
                        f"Code={cp.get('Code')}"
                    )
            return 0
    print(f"Class not found: {code}")
    return 1


def cmd_property(data: dict, code: str) -> int:
    for prop in data.get("Properties", []):
        if str(prop.get("Code", "")).lower() == code.lower():
            print(f"Code: {prop.get('Code')}")
            print(f"Name: {prop.get('Name')}")
            print(f"PropertyValueKind: {prop.get('PropertyValueKind')}")
            print(f"Description: {prop.get('Description')}")
            print(f"Definition: {prop.get('Definition')}")
            return 0
    print(f"Property not found: {code}")
    return 1


def cmd_find_class(data: dict, needle: str, limit: int) -> None:
    q = needle.lower()
    hits = []
    for cls in data.get("Classes", []):
        code = str(cls.get("Code", ""))
        name = str(cls.get("Name", ""))
        if q in code.lower() or q in name.lower():
            hits.append((code, name))
            if len(hits) >= limit:
                break
    for code, name in hits:
        print(f"{code}\t{name}")
    print(f"matches: {len(hits)}")


def cmd_find_property(data: dict, needle: str, limit: int) -> None:
    q = needle.lower()
    hits = []
    for prop in data.get("Properties", []):
        code = str(prop.get("Code", ""))
        name = str(prop.get("Name", ""))
        if q in code.lower() or q in name.lower():
            hits.append((code, name, prop.get("PropertyValueKind")))
            if len(hits) >= limit:
                break
    for code, name, kind in hits:
        print(f"{code}\t{name}\t{kind}")
    print(f"matches: {len(hits)}")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="Inspect local bSDD IFC 4.3 JSON export")
    p.add_argument("--file", default=DEFAULT_JSON, help="Path to bSDD JSON")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("summary")

    pc = sub.add_parser("class")
    pc.add_argument("code", help="Class code, e.g. IfcDoor")
    pc.add_argument("--limit-props", type=int, default=20)

    pp = sub.add_parser("property")
    pp.add_argument("code", help="Property code, e.g. OverallHeight")

    pfc = sub.add_parser("find-class")
    pfc.add_argument("query")
    pfc.add_argument("--limit", type=int, default=20)

    pfp = sub.add_parser("find-property")
    pfp.add_argument("query")
    pfp.add_argument("--limit", type=int, default=20)

    return p


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    data = load_dataset(Path(args.file))

    if args.cmd == "summary":
        cmd_summary(data)
        return 0
    if args.cmd == "class":
        return cmd_class(data, args.code, args.limit_props)
    if args.cmd == "property":
        return cmd_property(data, args.code)
    if args.cmd == "find-class":
        cmd_find_class(data, args.query, args.limit)
        return 0
    if args.cmd == "find-property":
        cmd_find_property(data, args.query, args.limit)
        return 0
    parser.error(f"Unhandled command: {args.cmd}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
