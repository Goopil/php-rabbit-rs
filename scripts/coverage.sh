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

for suite in \
    "Rust Core:coverage-rust.sh" \
    "PHP Extension:coverage-php-ext.sh" \
    "Laravel Package:coverage-laravel.sh"
do
    name="${suite%%:*}"
    script="${suite#*:}"
    echo ""
    echo "━━━ ${name} ━━━"
    if "${SCRIPT_DIR}/${script}"; then
        echo "  ✓ ${name} coverage OK"
    else
        echo "  ✗ ${name} coverage FAILED"
        FAILURES=$((FAILURES + 1))
    fi
done

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
