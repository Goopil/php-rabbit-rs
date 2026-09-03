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

# Unit and Feature tests use fake classes and must run WITHOUT the
# extension (see RabbitMqServiceProviderTest). Integration tests require
# ext-rabbit_rs loaded plus the RabbitMQ lab, so they run in a second pass
# with the artifact loaded via -d extension=<artifact>.
echo "Running Pest with coverage (Unit + Feature, no extension)..."
(
    cd "${PACKAGE_DIR}"
    "${PHP_BIN}" vendor/bin/pest \
        --coverage \
        --coverage-filter=src \
        --coverage-clover="${BUILD_DIR}/clover.xml" \
        --log-junit="${BUILD_DIR}/junit.xml" \
        --testdox
)

# --- Integration pass: requires docker + the RabbitMQ lab -------------------
LAB_STARTED=false
cleanup() {
    if [[ "${LAB_STARTED}" == true ]]; then
        echo ""
        echo "=== Stopping RabbitMQ lab ==="
        (cd "${ROOT}" && ./scripts/lab-down.sh) || true
    fi
}
trap cleanup EXIT

if ! command -v docker >/dev/null 2>&1 || ! docker ps >/dev/null 2>&1; then
    echo ""
    echo "SKIP: docker is not available, Integration coverage not collected" >&2
else
    LAB_STARTED=true

    echo ""
    echo "=== Starting RabbitMQ lab ==="
    (cd "${ROOT}" && ./scripts/lab-up.sh with-plugin)

    echo "=== Waiting for lab readiness ==="
    for i in $(seq 1 120); do
        if (cd "${ROOT}" && ./scripts/lab-ready.sh >/dev/null 2>&1); then
            break
        fi
        echo "  waiting for lab... (${i}s)"
        sleep 1
    done
    if ! (cd "${ROOT}" && ./scripts/lab-ready.sh >/dev/null 2>&1); then
        echo "ERROR: lab is not ready after 120s" >&2
        exit 1
    fi
    echo "Lab is ready."

    echo ""
    echo "=== Building ext-rabbit_rs ==="
    ext_ensure_built
    EXTENSION_SO="$(ext_artifact_path)"
    ext_verify_loads

    echo ""
    echo "Running Pest with coverage (Integration, extension loaded)..."
    (
        cd "${PACKAGE_DIR}"
        "${PHP_BIN}" -d "extension=${EXTENSION_SO}" vendor/bin/pest tests/Integration \
            --coverage \
            --coverage-filter=src \
            --coverage-clover="${BUILD_DIR}/clover-integration.xml" \
            --log-junit="${BUILD_DIR}/junit-integration.xml" \
            --testdox
    )
fi

echo ""
echo "Laravel coverage complete."
echo "  Clover:             ${BUILD_DIR}/clover.xml"
echo "  JUnit:              ${BUILD_DIR}/junit.xml"
if [[ -f "${BUILD_DIR}/clover-integration.xml" ]]; then
    echo "  Clover (Integr.):   ${BUILD_DIR}/clover-integration.xml"
    echo "  JUnit (Integr.):    ${BUILD_DIR}/junit-integration.xml"
fi
