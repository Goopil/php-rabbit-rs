#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COVERAGE_DIR="${ROOT}/target/coverage"

echo "=== Rust Coverage (nextest + llvm-cov) ==="

if ! command -v cargo-nextest >/dev/null 2>&1; then
    echo "ERROR: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
    exit 1
fi

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "ERROR: cargo-llvm-cov not found. Install with: cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

mkdir -p "${COVERAGE_DIR}"

echo "Running nextest with coverage..."
cargo llvm-cov nextest \
    --workspace \
    --all-targets \
    --no-fail-fast \
    --lcov \
    --output-path "${COVERAGE_DIR}/rust.lcov"

echo ""
echo "Generating HTML report..."
cargo llvm-cov report \
    --workspace \
    --all-targets \
    --html \
    --output-dir "${COVERAGE_DIR}/html"

echo ""
echo "Rust coverage complete."
echo "  LCOV: ${COVERAGE_DIR}/rust.lcov"
echo "  HTML: ${COVERAGE_DIR}/html/index.html"
