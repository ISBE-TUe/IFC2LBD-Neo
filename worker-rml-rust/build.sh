#!/bin/bash
# Build worker-rml-rust Docker image
#
# This script handles the build context setup, copying the rml_mapper
# library source into the context for the Docker build.
#
# Usage:
#   ./packages/worker-rml-rust/build.sh [--no-cache]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ONTOCORE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RML_MAPPER_SRC="${ONTOCORE_ROOT}/../rust_rml_mapper"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Building worker-rml-rust...${NC}"

# Check that rml_mapper exists
if [ ! -d "$RML_MAPPER_SRC" ]; then
    echo -e "${RED}Error: rust_rml_mapper not found at $RML_MAPPER_SRC${NC}"
    echo "Expected directory structure:"
    echo "  contstructX/"
    echo "    ├── ontocore/"
    echo "    └── rust_rml_mapper/"
    exit 1
fi

# Create temp build context
BUILD_CONTEXT=$(mktemp -d)
trap "rm -rf $BUILD_CONTEXT" EXIT

echo "Creating build context at $BUILD_CONTEXT"

# Copy rml_mapper (excluding cargo target dir and git, but NOT src/target module)
echo "Copying rust_rml_mapper..."
rsync -a --exclude='/target' --exclude='.git' "$RML_MAPPER_SRC/" "$BUILD_CONTEXT/rml_mapper/"

# Copy worker-rml-rust
echo "Copying worker-rml-rust..."
mkdir -p "$BUILD_CONTEXT/packages/worker-rml-rust"
cp -r "$SCRIPT_DIR/Cargo.toml" "$BUILD_CONTEXT/packages/worker-rml-rust/"
cp -r "$SCRIPT_DIR/src" "$BUILD_CONTEXT/packages/worker-rml-rust/"

# Build Docker image
echo -e "${YELLOW}Building Docker image...${NC}"

DOCKER_ARGS=""
if [ "$1" = "--no-cache" ]; then
    DOCKER_ARGS="--no-cache"
fi

docker build $DOCKER_ARGS \
    -f "$SCRIPT_DIR/Dockerfile" \
    -t ontocore-worker-rml-rust:latest \
    -t ontocore-worker-rml-rust:$(date +%Y%m%d) \
    "$BUILD_CONTEXT"

echo -e "${GREEN}✓ Build complete!${NC}"
echo ""
echo "Run with:"
echo "  docker run -p 8010:8000 ontocore-worker-rml-rust"
echo ""
echo "Or use docker-compose:"
echo "  docker-compose up worker-rml-rust"
