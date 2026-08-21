#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 1; }

cd "${PROJECT_ROOT}"

if ! docker ps >/dev/null 2>&1; then
    echo "ERROR: docker daemon is not running" >&2
    exit 1
fi

echo "=== Starting RabbitMQ lab ==="
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
PHP_BIN="${PHP_BIN:-php}"
if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "SKIP: php not found, cannot run Laravel integration tests"
else
    cargo build --manifest-path "${PROJECT_ROOT}/crates/rabbit-rs-php/Cargo.toml" --features extension-tests

    case "$(uname -s)" in
        Darwin) EXTENSION_SO="${PROJECT_ROOT}/target/debug/librabbit_rs_php.dylib" ;;
        Linux)  EXTENSION_SO="${PROJECT_ROOT}/target/debug/librabbit_rs_php.so" ;;
        *) echo "ERROR: unsupported OS: $(uname -s)" >&2; exit 1 ;;
    esac

    echo ""
    echo "=== Verifying extension is loaded ==="
    MODULES="$("${PHP_BIN}" -d "extension=${EXTENSION_SO}" -m 2>/dev/null || true)"
    if ! grep -q rabbit_rs <<< "${MODULES}"; then
        echo "ERROR: ext-rabbit_rs is not loaded" >&2
        echo "  PHP SAPI: $(${PHP_BIN} -r 'echo php_sapi_name();' 2>/dev/null)"
        echo "  PHP version: $(${PHP_BIN} -r 'echo phpversion();' 2>/dev/null)"
        exit 1
    fi
    echo "ext-rabbit_rs is loaded."

    echo ""
    echo "=== Installing Laravel package dependencies ==="
    cd "${PROJECT_ROOT}/packages/laravel-queue"
    composer update --quiet --ignore-platform-reqs

    echo ""
    echo "=== Running Laravel integration tests ==="
    "${PHP_BIN}" -d "extension=${EXTENSION_SO}" vendor/bin/pest tests/Integration --testdox
fi

echo ""
echo "=== Stopping RabbitMQ lab ==="
cd "${PROJECT_ROOT}"
./scripts/lab-down.sh

echo ""
echo "Integration tests complete."
