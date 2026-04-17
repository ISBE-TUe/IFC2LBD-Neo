#!/bin/bash
# Comprehensive benchmark: speed + memory + output size
# Usage: ./bench.sh [baseline|after-p0a|after-p0b|final] 
set -euo pipefail

LABEL="${1:-baseline}"
BIN="./target/release/ifc2lbd-neo"
OUTDIR="/tmp/ifc2lbd-bench/$LABEL"
mkdir -p "$OUTDIR"

# Fixtures
DH="DigitalHub_FM-ARC_v2.ifc"
WH="Wohn-Geschaeftshaus.ifc"

# Check binary exists
if [ ! -f "$BIN" ]; then
    echo "ERROR: $BIN not found. Build with: cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo"
    exit 1
fi

echo "============================================"
echo "  IFC2LBD-Neo Benchmark: $LABEL"
echo "  Binary: $($BIN --version 2>/dev/null || echo 'unknown')"
echo "  Time: $(date -Iseconds)"
echo "============================================"
echo ""

run_bench() {
    local FIXTURE="$1"
    local MODE="$2"
    local EXTRA_MODULES="$3"
    local OUTPUT="$OUTDIR/$(basename "$FIXTURE" .ifc)_${MODE}"
    shift 3
    local OUTPUT_ARG=""
    case "$MODE" in
        *-ttl) OUTPUT_ARG="--output ${OUTPUT}.ttl" ;;
        *-nq)  OUTPUT_ARG="--output ${OUTPUT}.nq"  ;;
    esac

    echo "--- $FIXTURE / $MODE ---"
    /usr/bin/time -l $BIN "$FIXTURE" $OUTPUT_ARG \
        --base-uri https://bench.example.com/ \
        --module neo-lbd-producer $EXTRA_MODULES \
        --module neo-file-export \
        2>&1 | tee "${OUTPUT}.log" \
        | grep -E "(phase |run completed|maximum resident|execution mode)" || true
    
    # Output file size
    for ext in ttl nq; do
        if [ -f "${OUTPUT}.${ext}" ]; then
            SIZE=$(wc -c < "${OUTPUT}.${ext}" | tr -d ' ')
            echo "  output: ${OUTPUT}.${ext} = ${SIZE} bytes"
        fi
    done
    echo ""
}

# DigitalHub benchmarks
run_bench "$DH" "lbd-ttl" "--module neo-turtle-serializer"
run_bench "$DH" "lbd-ifcowl-nq" "--module neo-ifcowl-producer --module neo-nquads-serializer"
run_bench "$DH" "lbd-ifcowl-ttl" "--module neo-ifcowl-producer --module neo-turtle-serializer"

# Wohn benchmarks
run_bench "$WH" "lbd-ttl" "--module neo-turtle-serializer"
run_bench "$WH" "lbd-ifcowl-nq" "--module neo-ifcowl-producer --module neo-nquads-serializer"
run_bench "$WH" "lbd-ifcowl-ttl" "--module neo-ifcowl-producer --module neo-turtle-serializer"
run_bench "$WH" "lbd-ifcowl-topofull-nq" "--module neo-ifcowl-producer --module neo-topology-full-producer --module neo-nquads-serializer"

# WASM crate unit tests (lowmem mode verification)
echo "=== WASM crate tests (lowmem mode) ==="
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo test -p ifc2lbd-wasm -- --nocapture 2>&1 | grep -E "(running|test |ok|FAILED)" || true
echo ""

# Pipeline crate tests
echo "=== Pipeline crate tests ==="
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo test -p lbd-pipeline -- --nocapture 2>&1 | grep -E "(running|test |ok|FAILED)" || true
echo ""

# Summary
echo "============================================"
echo "  Benchmark summary: $LABEL"
echo "============================================"
for log in "$OUTDIR"/*.log; do
    RUN=$(grep "run completed" "$log" | tail -1 | sed 's/.*run completed in //' | sed 's/s$//')
    RSS=$(grep "maximum resident" "$log" | tail -1 | awk '{print $1}')
    NAME=$(basename "$log" .log)
    if [ -n "$RUN" ] && [ -n "$RSS" ]; then
        RSS_MB=$((RSS / 1024 / 1024))
        printf "  %-45s %6ss  %4dMB\n" "$NAME" "$RUN" "$RSS_MB"
    fi
done
echo ""
echo "Logs: $OUTDIR/"
