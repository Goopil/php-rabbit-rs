#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

ROOT="$(ext_project_root)"
COVERAGE_DIR="${ROOT}/target/coverage"
PHP_EXT_DIR="${ROOT}/crates/rabbit-rs-php"
PHP_BIN="${PHP_BIN:-php}"

echo "=== PHP Extension Coverage (instrumented build + PHP tests) ==="

if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "ERROR: php not found" >&2
    exit 1
fi

mkdir -p "${COVERAGE_DIR}"

SUFFIX="$(_ext_suffix)"
ARTIFACT="${ROOT}/target/debug/librabbit_rs_php.${SUFFIX}"

echo "Building extension with coverage instrumentation..."
RUSTFLAGS="-Cinstrument-coverage -Clink-dead-code" \
    cargo build \
    --manifest-path "${ROOT}/crates/rabbit-rs-php/Cargo.toml" \
    --features extension-tests

if [[ ! -f "${ARTIFACT}" ]]; then
    echo "ERROR: extension artifact not found after build: ${ARTIFACT}" >&2
    exit 1
fi

if [[ ! -d "${PHP_EXT_DIR}/vendor" ]]; then
    echo "Installing PHP extension dependencies..."
    (cd "${PHP_EXT_DIR}" && composer update --no-interaction --no-security-blocking)
fi

echo "Running Pest tests with coverage instrumentation..."
(
    cd "${PHP_EXT_DIR}"
    LLVM_PROFILE_FILE="${ROOT}/target/debug/rabbit-rs-php-%p.profraw" \
        "${PHP_BIN}" -d "extension=${ARTIFACT}" vendor/bin/pest \
        --log-junit="${COVERAGE_DIR}/php-ext-junit.xml"
)

echo "Merging profraw files..."
llvm-profdata merge \
    -sparse \
    "${ROOT}"/target/debug/rabbit-rs-php-*.profraw \
    -o "${COVERAGE_DIR}/php-ext.profdata"

echo "Exporting LCOV (excluding rabbit-rs-core)..."
llvm-cov export \
    "${ARTIFACT}" \
    -instr-profile="${COVERAGE_DIR}/php-ext.profdata" \
    --format lcov \
    --ignore-filename-regex='rabbit-rs-core' \
    > "${COVERAGE_DIR}/php-ext.lcov"

echo ""
echo "PHP extension coverage complete."
echo "  LCOV: ${COVERAGE_DIR}/php-ext.lcov"
echo "  JUnit: ${COVERAGE_DIR}/php-ext-junit.xml"
