#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COVERAGE_DIR="${ROOT}/target/coverage"

echo ""
echo "╔══════════════════════════════════════════════╗"
echo "║         Rabbit RS — Full Coverage Run        ║"
echo "╚══════════════════════════════════════════════╝"
echo ""

mkdir -p "${COVERAGE_DIR}"

FAILURES=0

echo "━━━ Rust Core ━━━"
if "${SCRIPT_DIR}/coverage-rust.sh"; then
    echo "  ✓ Rust coverage OK"
else
    echo "  ✗ Rust coverage FAILED"
    FAILURES=$((FAILURES + 1))
fi

echo ""
echo "━━━ PHP Extension ━━━"
if "${SCRIPT_DIR}/coverage-php-ext.sh"; then
    echo "  ✓ PHP extension coverage OK"
else
    echo "  ✗ PHP extension coverage FAILED"
    FAILURES=$((FAILURES + 1))
fi

echo ""
echo "━━━ Laravel Package ━━━"
if "${SCRIPT_DIR}/coverage-laravel.sh"; then
    echo "  ✓ Laravel coverage OK"
else
    echo "  ✗ Laravel coverage FAILED"
    FAILURES=$((FAILURES + 1))
fi

echo ""
echo "━━━ Summary ━━━"
if [[ ${FAILURES} -eq 0 ]]; then
    echo "All coverage runs succeeded."
    echo ""
    echo "Reports:"
    echo "  Rust LCOV:        ${COVERAGE_DIR}/rust.lcov"
    echo "  Rust HTML:        ${COVERAGE_DIR}/html/index.html"
    echo "  PHP ext LCOV:     ${COVERAGE_DIR}/php-ext.lcov"
    echo "  Laravel Clover:   ${ROOT}/packages/laravel-queue/build/clover.xml"
    echo ""
    if [[ -f "${COVERAGE_DIR}/html/index.html" ]]; then
        echo "Open HTML report: file://${COVERAGE_DIR}/html/index.html"
    fi
    exit 0
else
    echo "${FAILURES} coverage run(s) failed." >&2
    exit 1
fi
