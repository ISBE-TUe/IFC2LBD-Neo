#!/bin/bash
# Bootstrap the rust_rml_mapper sibling repository.
#
# worker-rml-rust depends on `rml_mapper` via a Cargo path dependency:
#     rml_mapper = { path = "../../../rust_rml_mapper" }
#
# That path is resolved RELATIVE to packages/worker-rml-rust/, meaning it
# expects a `rust_rml_mapper/` checkout as a SIBLING of the `ontocore/`
# monorepo root, e.g.:
#
#     contstructX/
#       ├── ontocore/             ← this repo
#       └── rust_rml_mapper/      ← cloned by this script
#
# The Docker build copies this sibling into a temp build context via
# `build.sh`. For local `cargo check` / editor integration, run this script
# once after cloning ontocore and it will clone the sibling if missing.
#
# Usage:
#   ./packages/worker-rml-rust/bootstrap.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ONTOCORE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PARENT_DIR="$(cd "$ONTOCORE_ROOT/.." && pwd)"
RML_MAPPER_DIR="$PARENT_DIR/rust_rml_mapper"

# Override with RUST_RML_MAPPER_REPO if you fork
RML_MAPPER_REPO="${RUST_RML_MAPPER_REPO:-https://github.com/RMLio/rust_rml_mapper.git}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

if [ -d "$RML_MAPPER_DIR" ]; then
    echo -e "${GREEN}✓ rust_rml_mapper already present at: $RML_MAPPER_DIR${NC}"
    exit 0
fi

echo -e "${YELLOW}rust_rml_mapper not found at: $RML_MAPPER_DIR${NC}"
echo "Cloning from $RML_MAPPER_REPO ..."

if ! command -v git > /dev/null 2>&1; then
    echo -e "${RED}Error: git is required but not installed.${NC}" >&2
    exit 1
fi

git clone "$RML_MAPPER_REPO" "$RML_MAPPER_DIR"

echo -e "${GREEN}✓ rust_rml_mapper cloned to: $RML_MAPPER_DIR${NC}"
echo ""
echo "You can now run:"
echo "  cargo check  (in packages/worker-rml-rust/)"
echo "  make rml-build"
