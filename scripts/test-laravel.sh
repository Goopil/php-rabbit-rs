#!/usr/bin/env bash
set -euo pipefail

# Run Laravel Pest tests. Unit and Feature tests use fake classes
# (no extension needed). Integration tests require ext-rabbit_rs.
#
# Usage:
#   ./scripts/test-laravel.sh                     # Unit + Feature (no extension)
#   ./scripts/test-laravel.sh --with-extension    # Unit + Feature (with extension)
#   ./scripts/test-laravel.sh tests/Integration   # specified paths (auto-builds extension)
#   ./scripts/test-laravel.sh --testdox            # passes extra args to Pest

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PACKAGE_DIR="${PROJECT_ROOT}/packages/laravel-queue"

PHP_BIN="${PHP_BIN:-php}"

if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "ERROR: php not found" >&2
    exit 1
fi

# --- Parse args: separate our flags from Pest args ---
WITH_EXTENSION=false
PEST_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-extension)
            WITH_EXTENSION=true
            shift
            ;;
        *)
            PEST_ARGS+=("$1")
            shift
            ;;
    esac
done

# --- Determine if we need the extension ---
NEED_EXTENSION=false
if [[ "${WITH_EXTENSION}" == true ]]; then
    NEED_EXTENSION=true
fi
for arg in "${PEST_ARGS[@]:-}"; do
    case "${arg}" in
        tests/Integration|tests/Integration/*)
            NEED_EXTENSION=true
            ;;
    esac
done

PHP_CMD=("${PHP_BIN}")

if [[ "${NEED_EXTENSION}" == true ]]; then
    # --- Detect extension artifact path ---
    case "$(uname -s)" in
        Darwin)
            ARTIFACT="${PROJECT_ROOT}/target/release/librabbit_rs_php.dylib"
            ;;
        Linux)
            ARTIFACT="${PROJECT_ROOT}/target/release/librabbit_rs_php.so"
            ;;
        *)
            echo "ERROR: unsupported operating system: $(uname -s)" >&2
            exit 1
            ;;
    esac

    # --- Build extension if artifact is missing ---
    if [[ ! -f "${ARTIFACT}" ]]; then
        echo "=== Building ext-rabbit_rs ==="
        cargo build --release \
            --manifest-path "${PROJECT_ROOT}/crates/rabbit-rs-php/Cargo.toml" \
            --features extension-tests
    fi

    if [[ ! -f "${ARTIFACT}" ]]; then
        echo "ERROR: extension artifact not found after build: ${ARTIFACT}" >&2
        exit 1
    fi

    # --- Verify extension loads ---
    echo "=== Verifying ext-rabbit_rs loads ==="
    MODULES="$("${PHP_BIN}" -d "extension=${ARTIFACT}" -m 2>/dev/null || true)"
    if ! grep -q rabbit_rs <<< "${MODULES}"; then
        echo "ERROR: ext-rabbit_rs failed to load" >&2
        echo "  PHP SAPI: $(${PHP_BIN} -r 'echo php_sapi_name();' 2>/dev/null)"
        echo "  PHP version: $(${PHP_BIN} -r 'echo phpversion();' 2>/dev/null)"
        exit 1
    fi
    echo "ext-rabbit_rs is loaded."

    PHP_CMD=("${PHP_BIN}" -d "extension=${ARTIFACT}")
fi

# --- Install Laravel package dependencies if needed ---
if [[ ! -d "${PACKAGE_DIR}/vendor" ]]; then
    echo ""
    echo "=== Installing Laravel package dependencies ==="
    (cd "${PACKAGE_DIR}" && composer install --quiet --ignore-platform-reqs)
fi

# --- Run Pest tests ---
echo ""
if [[ "${NEED_EXTENSION}" == true ]]; then
    echo "=== Running Laravel Pest tests (with extension) ==="
else
    echo "=== Running Laravel Pest tests (fake classes, no extension) ==="
fi
cd "${PACKAGE_DIR}"

if [[ ${#PEST_ARGS[@]} -eq 0 ]]; then
    "${PHP_CMD[@]}" vendor/bin/pest tests/Unit tests/Feature --testdox
else
    "${PHP_CMD[@]}" vendor/bin/pest "${PEST_ARGS[@]}"
fi

echo ""
echo "Laravel Pest tests complete."
