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
#
# The -n flag is used to ignore system ini files, preventing double-loading
# of the extension when it is already installed system-wide.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

PROJECT_ROOT="$(ext_project_root)"
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

# --- Build the appropriate PHP command ---
# Unit/Feature tests use php -n (no ini, no extension) so the
# "missing extension" assertion in RabbitMqServiceProviderTest passes.
# Integration tests use php -n -d extension=<local artifact>.
if [[ "${NEED_EXTENSION}" == true ]]; then
    ext_ensure_built
    ext_verify_loads
    # shellcheck disable=SC2207
    PHP_CMD=( $(ext_php_cmd) )
else
    # shellcheck disable=SC2207
    PHP_CMD=( $(ext_php_no_ext_cmd) )
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
    "${PHP_CMD[@]}" vendor/bin/pest --testdox
else
    "${PHP_CMD[@]}" vendor/bin/pest "${PEST_ARGS[@]}"
fi

echo ""
echo "Laravel Pest tests complete."
