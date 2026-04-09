#!/usr/bin/env python3

from __future__ import annotations

import argparse
import re
from pathlib import Path


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(
        description="Scaffold a producer plugin crate from templates."
    )
    parser.add_argument("--id", required=True, help="Plugin short id, e.g. voxels")
    parser.add_argument(
        "--display-name", required=True, help='Display name, e.g. "Voxel Producer"'
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=root,
        help="Repository root (defaults to project root).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print generated file paths without writing files.",
    )
    return parser.parse_args()


def validate_plugin_id(plugin_id: str) -> str:
    normalized = plugin_id.strip().lower()
    if not re.fullmatch(r"[a-z][a-z0-9-]*", normalized):
        raise ValueError(
            "invalid --id, expected lowercase kebab-case starting with a letter"
        )
    return normalized


def render(template: str, values: dict[str, str]) -> str:
    out = template
    for key, value in values.items():
        out = out.replace(f"{{{{{key}}}}}", value)
    return out


def main() -> None:
    args = parse_args()
    plugin_id = validate_plugin_id(args.id)
    crate_name = f"plugin-{plugin_id}"
    type_name = "".join(part.capitalize() for part in plugin_id.split("-")) + "Plugin"
    plugin_manifest_id = f"custom-{plugin_id}-producer"
    crate_dir = args.root / "crates" / crate_name

    values = {
        "crate_name": crate_name,
        "plugin_id": plugin_manifest_id,
        "display_name": args.display_name.strip(),
        "type_name": type_name,
    }

    template_root = args.root / "templates" / "plugin-producer"
    files = {
        crate_dir / "Cargo.toml": render(
            (template_root / "Cargo.toml.template").read_text(), values
        ),
        crate_dir / "src" / "lib.rs": render(
            (template_root / "lib.rs.template").read_text(), values
        ),
        crate_dir / "README.md": render(
            (template_root / "README.md.template").read_text(), values
        ),
    }

    if args.dry_run:
        for path in files:
            print(path)
        return

    for path, content in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists():
            raise FileExistsError(f"refusing to overwrite existing file: {path}")
        path.write_text(content)
        print(f"created {path}")

    print()
    print("Next steps:")
    print(f"1. Add `{crate_dir.relative_to(args.root)}` to workspace members in Cargo.toml.")
    print("2. Register the plugin manifest in crates/ifc2lbd-cli/src/pipeline_plugins.rs.")
    print("3. Add one executor entry in crates/ifc2lbd-cli/src/topology_plugin.rs (TOPOLOGY_EXECUTORS).")
    print(f"4. Validate with: cargo check -p {crate_name} && cargo check -p ifc2lbd-cli")


if __name__ == "__main__":
    main()
