#!/usr/bin/env python3
"""Extract the BEO declared-class allowlist from the vendored ontology.

Reads `ontologies/beo.ttl` and writes the sorted local names of every class
declared in the BEO namespace to `crates/lbd-converter/resources/beo_classes.txt`,
one per line.

`lbd-converter` embeds that list with `include_str!` and consults it before
emitting any `beo:` product-class type, so a type is only ever emitted when BEO
actually declares it. Embedding the list rather than the 137 KB Turtle keeps the
WASM binary small and avoids a parse at first use.

Regenerate after bumping the vendored ontology:

    python3 scripts/build_beo_index.py

Requires no third-party packages: the BEO source uses a single uniform
declaration form, so a targeted line scan is both sufficient and easier to audit
than pulling in an RDF toolchain. The script fails loudly if that assumption ever
stops holding (see `EXPECTED_PRESENT` / `EXPECTED_ABSENT` and the count floor).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ONTOLOGY = REPO_ROOT / "ontologies" / "beo.ttl"
OUTPUT = REPO_ROOT / "crates" / "lbd-converter" / "resources" / "beo_classes.txt"

BEO_NAMESPACE = "https://pi.pauwel.be/voc/buildingelement#"

# A BEO local name: leading letter, then letters/digits/underscore/hyphen.
# Hyphens matter — the predefined-type variants are `Railing-HANDRAIL` etc.
LOCAL_NAME = r"[A-Za-z][A-Za-z0-9_-]*"

# `:Railing rdf:type owl:Class ;` — the form BEO actually uses.
PREFIXED_DECL = re.compile(rf"^\s*:({LOCAL_NAME})\s+(?:rdf:type|a)\s+owl:Class\b")

# `<https://pi.pauwel.be/voc/buildingelement#Railing> a owl:Class ;` — not used by
# BEO today, accepted so a reserialisation of the source does not silently yield an
# empty allowlist.
ABSOLUTE_DECL = re.compile(
    rf"^\s*<{re.escape(BEO_NAMESPACE)}({LOCAL_NAME})>\s+(?:rdf:type|a)\s+owl:Class\b"
)

# Guards against a malformed regeneration silently shipping a wrong allowlist.
# These are the exact cases the vocabulary audit turned on.
EXPECTED_PRESENT = (
    "Railing",
    "Stair",
    "Roof",
    "Slab",
    "BuildingElement",
    "Railing-BALUSTRADE",
    "Railing-GUARDRAIL",
    "Railing-HANDRAIL",
)
EXPECTED_ABSENT = (
    "Railing-NOTDEFINED",
    "Stair-NOTDEFINED",
    "Roof-NOTDEFINED",
    "Slab-NOTDEFINED",
    "BuildingElement-NOTDEFINED",
    "Furniture",
)
MIN_CLASS_COUNT = 150


def extract_classes(text: str) -> set[str]:
    names: set[str] = set()
    for line in text.splitlines():
        match = PREFIXED_DECL.match(line) or ABSOLUTE_DECL.match(line)
        if match:
            names.add(match.group(1))
    return names


def main() -> int:
    if not ONTOLOGY.is_file():
        print(f"error: vendored ontology missing at {ONTOLOGY}", file=sys.stderr)
        return 1

    classes = extract_classes(ONTOLOGY.read_text(encoding="utf-8"))

    if len(classes) < MIN_CLASS_COUNT:
        print(
            f"error: extracted only {len(classes)} classes from {ONTOLOGY.name}, "
            f"expected at least {MIN_CLASS_COUNT}. The declaration syntax likely "
            f"changed — fix the patterns in this script rather than lowering the floor.",
            file=sys.stderr,
        )
        return 1

    missing = [name for name in EXPECTED_PRESENT if name not in classes]
    if missing:
        print(
            f"error: expected classes absent from the extraction: {', '.join(missing)}",
            file=sys.stderr,
        )
        return 1

    unexpected = [name for name in EXPECTED_ABSENT if name in classes]
    if unexpected:
        print(
            f"error: BEO now declares {', '.join(unexpected)}. That invalidates an "
            f"assumption this converter relies on — review the emission guard in "
            f"crates/lbd-converter/src/beo_index.rs before regenerating.",
            file=sys.stderr,
        )
        return 1

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text("\n".join(sorted(classes)) + "\n", encoding="utf-8")
    print(f"wrote {len(classes)} BEO class names to {OUTPUT.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
