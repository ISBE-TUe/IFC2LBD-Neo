#!/bin/bash
# Benchmark: Java RML Worker vs Rust RML Worker
#
# Prerequisites:
#   1. Both workers running:
#      - Java: docker-compose up -d worker-rml (port 8001)
#      - Rust: docker-compose --profile rust up -d worker-rml-rust (port 8010)
#   2. Build Rust worker first: ./packages/worker-rml-rust/build.sh
#
# Usage:
#   ./packages/worker-rml-rust/benchmark.sh [records]
#
# Example:
#   ./packages/worker-rml-rust/benchmark.sh 10000

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ONTOCORE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Configuration
NUM_RECORDS=${1:-1000}
JAVA_URL="http://localhost:8001"
RUST_URL="http://localhost:8010"
MAPPING_FILE="$ONTOCORE_ROOT/app/mappings/generic-json.rml.ttl"
TMP_DIR=$(mktemp -d)

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${CYAN}  RML Worker Benchmark: Java vs Rust${NC}"
echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# Check workers are running
echo -e "${BLUE}Checking worker availability...${NC}"

JAVA_HEALTH=$(curl -s -o /dev/null -w "%{http_code}" "$JAVA_URL/healthz" 2>/dev/null || echo "000")
RUST_HEALTH=$(curl -s -o /dev/null -w "%{http_code}" "$RUST_URL/healthz" 2>/dev/null || echo "000")

if [ "$JAVA_HEALTH" != "200" ]; then
    echo -e "${YELLOW}⚠ Java worker not available at $JAVA_URL${NC}"
    echo "  Start with: docker-compose up -d worker-rml"
    JAVA_AVAILABLE=false
else
    echo -e "${GREEN}✓ Java worker ready${NC}"
    JAVA_AVAILABLE=true
fi

if [ "$RUST_HEALTH" != "200" ]; then
    echo -e "${YELLOW}⚠ Rust worker not available at $RUST_URL${NC}"
    echo "  Build: ./packages/worker-rml-rust/build.sh"
    echo "  Start: docker-compose --profile rust up -d worker-rml-rust"
    RUST_AVAILABLE=false
else
    RUST_INFO=$(curl -s "$RUST_URL/healthz" | jq -r '.version // "unknown"')
    echo -e "${GREEN}✓ Rust worker ready (v$RUST_INFO)${NC}"
    RUST_AVAILABLE=true
fi

if [ "$JAVA_AVAILABLE" = "false" ] && [ "$RUST_AVAILABLE" = "false" ]; then
    echo -e "${RED}Error: No workers available${NC}"
    exit 1
fi

echo ""

# Generate test data
echo -e "${BLUE}Generating test data ($NUM_RECORDS records)...${NC}"
DATA_FILE="$TMP_DIR/data.json"

python3 - "$NUM_RECORDS" "$DATA_FILE" << 'PYTHON'
import json
import sys
from datetime import datetime, timedelta
import random

num_records = int(sys.argv[1])
output_file = sys.argv[2]

categories = ["Technology", "Science", "Business", "Health", "Education"]
statuses = ["active", "pending", "completed", "archived"]

data = []
for i in range(num_records):
    record = {
        "id": f"item-{i+1:06d}",
        "name": f"Test Item {i+1}",
        "description": f"This is a detailed description for test item number {i+1}. " * 3,
        "type": "Thing",
        "url": f"http://example.org/item/{i+1}",
        "category": random.choice(categories),
        "status": random.choice(statuses),
        "value": str(random.randint(1, 10000)),
        "identifier": f"ID-{i+1:06d}",
        "dateCreated": (datetime.now() - timedelta(days=random.randint(1, 365))).isoformat() + "Z",
        "dateModified": datetime.now().isoformat() + "Z",
        "author": f"Author {random.randint(1, 100)}",
        "keywords": ", ".join(random.sample(["tech", "science", "data", "ai", "cloud", "web"], 3))
    }
    data.append(record)

with open(output_file, 'w') as f:
    json.dump(data, f)

print(f"Generated {num_records} records ({len(json.dumps(data))} bytes)")
PYTHON

DATA_SIZE=$(stat -f%z "$DATA_FILE" 2>/dev/null || stat -c%s "$DATA_FILE")
echo -e "  Data file: $(numfmt --to=iec $DATA_SIZE 2>/dev/null || echo "${DATA_SIZE} bytes")"
echo ""

# Benchmark function
run_benchmark() {
    local name=$1
    local url=$2
    local output_file="$TMP_DIR/${name}_output.ttl"
    local timing_file="$TMP_DIR/${name}_timing.txt"
    
    echo -e "${BLUE}Running $name benchmark...${NC}"
    
    # Run with timing
    START_TIME=$(python3 -c "import time; print(time.time())")
    
    RESPONSE=$(curl -s -w "\n%{http_code}" \
        -X POST "$url/execute" \
        -F "file=@$DATA_FILE;filename=data.json" \
        -F "mapping=@$MAPPING_FILE;filename=mapping.rml.ttl" \
        -F "output_format=turtle" \
        2>&1)
    
    END_TIME=$(python3 -c "import time; print(time.time())")
    
    HTTP_CODE=$(echo "$RESPONSE" | tail -n1)
    BODY=$(echo "$RESPONSE" | sed '$d')
    
    if [ "$HTTP_CODE" != "200" ]; then
        echo -e "${RED}  ✗ Failed (HTTP $HTTP_CODE)${NC}"
        echo "$BODY" | head -5
        return 1
    fi
    
    # Extract metrics
    ELAPSED=$(python3 -c "print(f'{($END_TIME - $START_TIME) * 1000:.1f}')")
    TRIPLE_COUNT=$(echo "$BODY" | jq -r '.triple_count_estimate // 0')
    RDF_SIZE=$(echo "$BODY" | jq -r '.rdf' | wc -c)
    
    # Save output for verification
    echo "$BODY" | jq -r '.rdf' > "$output_file"
    
    echo -e "  ${GREEN}✓ Success${NC}"
    echo "  Time: ${ELAPSED}ms"
    echo "  Triples: $TRIPLE_COUNT"
    echo "  Output: $(numfmt --to=iec $RDF_SIZE 2>/dev/null || echo "${RDF_SIZE} bytes")"
    
    # Store results for comparison
    echo "$ELAPSED" > "$timing_file"
    echo "$TRIPLE_COUNT" >> "$timing_file"
}

# Run benchmarks
RESULTS=""
echo -e "${CYAN}─────────────────────────────────────────────────────────────────${NC}"

if [ "$JAVA_AVAILABLE" = "true" ]; then
    if run_benchmark "java" "$JAVA_URL"; then
        JAVA_TIME=$(head -1 "$TMP_DIR/java_timing.txt")
        JAVA_TRIPLES=$(tail -1 "$TMP_DIR/java_timing.txt")
    fi
    echo ""
fi

if [ "$RUST_AVAILABLE" = "true" ]; then
    if run_benchmark "rust" "$RUST_URL"; then
        RUST_TIME=$(head -1 "$TMP_DIR/rust_timing.txt")
        RUST_TRIPLES=$(tail -1 "$TMP_DIR/rust_timing.txt")
    fi
    echo ""
fi

# Summary
echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${CYAN}  Results Summary${NC}"
echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
echo ""
echo "Records processed: $NUM_RECORDS"
echo ""

if [ -n "$JAVA_TIME" ] && [ -n "$RUST_TIME" ]; then
    SPEEDUP=$(python3 -c "print(f'{$JAVA_TIME / $RUST_TIME:.1f}x')")
    
    printf "%-12s %12s %12s %12s\n" "" "Java" "Rust" "Speedup"
    printf "%-12s %12s %12s %12s\n" "─────────" "─────────" "─────────" "─────────"
    printf "%-12s %10.1fms %10.1fms %12s\n" "Time" "$JAVA_TIME" "$RUST_TIME" "$SPEEDUP"
    printf "%-12s %12s %12s %12s\n" "Triples" "$JAVA_TRIPLES" "$RUST_TRIPLES" ""
    
    echo ""
    if (( $(echo "$RUST_TIME < $JAVA_TIME" | bc -l) )); then
        echo -e "${GREEN}🚀 Rust is ${SPEEDUP} faster!${NC}"
    else
        echo -e "${YELLOW}Java was faster this time${NC}"
    fi
    
    # Verify outputs match
    if [ -f "$TMP_DIR/java_output.ttl" ] && [ -f "$TMP_DIR/rust_output.ttl" ]; then
        JAVA_LINES=$(wc -l < "$TMP_DIR/java_output.ttl")
        RUST_LINES=$(wc -l < "$TMP_DIR/rust_output.ttl")
        if [ "$JAVA_LINES" = "$RUST_LINES" ]; then
            echo -e "${GREEN}✓ Output line counts match ($JAVA_LINES lines)${NC}"
        else
            echo -e "${YELLOW}⚠ Output line counts differ (Java: $JAVA_LINES, Rust: $RUST_LINES)${NC}"
        fi
    fi
elif [ -n "$JAVA_TIME" ]; then
    printf "%-12s %12s\n" "" "Java"
    printf "%-12s %12s\n" "─────────" "─────────"
    printf "%-12s %10.1fms\n" "Time" "$JAVA_TIME"
    printf "%-12s %12s\n" "Triples" "$JAVA_TRIPLES"
elif [ -n "$RUST_TIME" ]; then
    printf "%-12s %12s\n" "" "Rust"
    printf "%-12s %12s\n" "─────────" "─────────"
    printf "%-12s %10.1fms\n" "Time" "$RUST_TIME"
    printf "%-12s %12s\n" "Triples" "$RUST_TRIPLES"
fi

echo ""
echo -e "${CYAN}─────────────────────────────────────────────────────────────────${NC}"
