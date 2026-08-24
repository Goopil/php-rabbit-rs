#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

ROOT="$(ext_project_root)"
PACKAGE_DIR="${ROOT}/packages/laravel-queue"
BUILD_DIR="${PACKAGE_DIR}/build"
PHP_BIN="${PHP_BIN:-php}"

echo "=== Laravel Package Coverage (Pest + PCOV) ==="

if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "ERROR: php not found" >&2
    exit 1
fi

mkdir -p "${BUILD_DIR}"

echo "Building extension (release)..."
cargo build --release --manifest-path "${ROOT}/crates/rabbit-rs-php/Cargo.toml"

SUFFIX="$(_ext_suffix)"
ARTIFACT="${ROOT}/target/release/librabbit_rs_php.${SUFFIX}"

if [[ ! -f "${ARTIFACT}" ]]; then
    echo "ERROR: extension artifact not found: ${ARTIFACT}" >&2
    exit 1
fi

if [[ ! -d "${PACKAGE_DIR}/vendor" ]]; then
    echo "Installing Laravel package dependencies..."
    (cd "${PACKAGE_DIR}" && composer install --quiet --ignore-platform-reqs)
fi

echo "Running Pest with coverage..."
(
    cd "${PACKAGE_DIR}"
    "${PHP_BIN}" vendor/bin/pest \
        --coverage \
        --coverage-clover="${BUILD_DIR}/clover.xml" \
        --log-junit="${BUILD_DIR}/junit.xml" \
        --testdox
)

echo ""
echo "Laravel coverage complete."
echo "  Clover: ${BUILD_DIR}/clover.xml"
echo "  JUnit:  ${BUILD_DIR}/junit.xml"
