#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

PROJECT_ROOT="$(ext_project_root)"

command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 1; }

# Stop the lab on success and on any failure, mirroring test-fpm.sh.
LAB_STARTED=false
cleanup() {
    if [[ "${LAB_STARTED}" == true ]]; then
        echo ""
        echo "=== Stopping RabbitMQ lab ==="
        cd "${PROJECT_ROOT}"
        ./scripts/lab-down.sh || true
    fi
}
trap cleanup EXIT

cd "${PROJECT_ROOT}"

if ! docker ps >/dev/null 2>&1; then
    echo "ERROR: docker daemon is not running" >&2
    exit 1
fi

echo "=== Starting RabbitMQ lab ==="
LAB_STARTED=true
./scripts/lab-up.sh with-plugin

echo "=== Waiting for lab readiness ==="
for i in $(seq 1 120); do
    if ./scripts/lab-ready.sh >/dev/null 2>&1; then
        break
    fi
    echo "  waiting for lab... (${i}s)"
    sleep 1
done

if ! ./scripts/lab-ready.sh >/dev/null 2>&1; then
    echo "ERROR: lab is not ready after 120s" >&2
    exit 1
fi
echo "Lab is ready."

echo ""
echo "=== Running Rust integration tests ==="
cargo test -p rabbit-rs-core --features integration --test integration -- --test-threads=1

echo ""
echo "=== Building ext-rabbit_rs ==="
ext_ensure_built
EXTENSION_SO="$(ext_artifact_path)"

PHP_BIN="${PHP_BIN:-php}"
if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "SKIP: php not found, cannot run Laravel integration tests"
else
    echo ""
    ext_verify_loads

    echo ""
    echo "=== Installing Laravel package dependencies ==="
    cd "${PROJECT_ROOT}/packages/laravel-queue"
    composer update --quiet --ignore-platform-reqs

    echo ""
    echo "=== Running Laravel integration tests ==="
    "${PHP_BIN}" -d "extension=${EXTENSION_SO}" vendor/bin/pest tests/Integration --testdox
fi

echo ""
echo "Integration tests complete."
