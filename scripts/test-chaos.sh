#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 1; }
command -v curl  >/dev/null 2>&1 || { echo "ERROR: curl is required"  >&2; exit 1; }
command -v jq    >/dev/null 2>&1 || { echo "ERROR: jq is required"    >&2; exit 1; }

cd "${PROJECT_ROOT}"

if ! docker ps >/dev/null 2>&1; then
    echo "ERROR: docker daemon is not running" >&2
    exit 1
fi

LAB_UP=true
if ! docker ps --format '{{.Names}}' 2>/dev/null | grep -q rabbitrs-rabbitmq-1; then
    LAB_UP=false
fi

if [[ "${LAB_UP}" == false ]]; then
    echo "=== Starting RabbitMQ lab (with-plugin profile) ==="
    ./scripts/lab-up.sh with-plugin
fi

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

# Reset Toxiproxy to a clean state.
echo "=== Resetting Toxiproxy ==="
curl -sf -X POST http://localhost:8474/reset >/dev/null 2>&1 || true
echo "Toxiproxy reset."

echo ""
echo "=== Running Rust chaos tests ==="
# Run chaos tests one at a time to avoid interference between scenarios.
RUST_CHAOS_PASSED=0
RUST_CHAOS_FAILED=0

CHAOS_TESTS=(
    "chaos_tcp_reset_before_confirm"
    "chaos_tcp_reset_after_confirm_before_ack"
    "chaos_quorum_leader_shutdown"
    "chaos_node_restart"
    "chaos_consumer_partition"
    "chaos_channel_closed_topology_error"
    "chaos_delay_plugin_unavailable"
    "chaos_credentials_rejected"
    "chaos_worker_sigterm_with_unacked"
)

for test_name in "${CHAOS_TESTS[@]}"; do
    echo ""
    echo "--- ${test_name} ---"
    # Reset Toxiproxy between tests.
    curl -sf -X POST http://localhost:8474/reset >/dev/null 2>&1 || true
    if cargo test -p rabbit-rs-core --features integration --test chaos_reconnect "${test_name}" -- --exact --test-threads=1 2>&1; then
        echo "PASS: ${test_name}"
        RUST_CHAOS_PASSED=$((RUST_CHAOS_PASSED + 1))
    else
        echo "FAIL: ${test_name}"
        RUST_CHAOS_FAILED=$((RUST_CHAOS_FAILED + 1))
    fi
    # Clean up between tests.
    curl -sf -X POST http://localhost:8474/reset >/dev/null 2>&1 || true
done

echo ""
echo "=== Rust chaos tests: ${RUST_CHAOS_PASSED} passed, ${RUST_CHAOS_FAILED} failed ==="

LARAVEL_CHAOS_PASSED=0
LARAVEL_CHAOS_FAILED=0

PHP_BIN="${PHP_BIN:-php}"
if ! command -v "${PHP_BIN}" >/dev/null 2>&1; then
    echo "SKIP: php not found, cannot run Laravel chaos tests"
else
    echo ""
    echo "=== Building ext-rabbit_rs ==="
    EXTENSION_SO=""
    if cargo build --release --manifest "${PROJECT_ROOT}/crates/rabbit-rs-php/Cargo.toml" 2>&1; then
        EXTENSION_SO="${PROJECT_ROOT}/target/release/librabbit_rs_php.so"
        echo "ext-rabbit_rs built: ${EXTENSION_SO}"
    else
        echo "WARN: could not build ext-rabbit_rs, skipping Laravel chaos tests"
    fi

    if [[ -n "${EXTENSION_SO}" ]] && "${PHP_BIN}" -d "extension=${EXTENSION_SO}" -m 2>/dev/null | grep -q rabbit_rs; then
        echo ""
        echo "=== Installing Laravel package dependencies ==="
        cd "${PROJECT_ROOT}/packages/laravel-queue"
        composer install --quiet --ignore-platform-reqs 2>/dev/null || true

        echo ""
        echo "=== Running Laravel chaos tests ==="
        curl -sf -X POST http://localhost:8474/reset >/dev/null 2>&1 || true
        if "${PHP_BIN}" -d "extension=${EXTENSION_SO}" vendor/bin/phpunit --testsuite="Rabbit RS Integration" --filter="AtLeastOnceChaosTest" --testdox 2>&1; then
            echo "PASS: Laravel chaos tests"
            LARAVEL_CHAOS_PASSED=$((LARAVEL_CHAOS_PASSED + 1))
        else
            echo "FAIL: Laravel chaos tests"
            LARAVEL_CHAOS_FAILED=$((LARAVEL_CHAOS_FAILED + 1))
        fi
        curl -sf -X POST http://localhost:8474/reset >/dev/null 2>&1 || true
    else
        echo "SKIP: ext-rabbit_rs is not loaded"
    fi
fi

echo ""
echo "=== Chaos test summary ==="
echo "Rust:     ${RUST_CHAOS_PASSED} passed, ${RUST_CHAOS_FAILED} failed"
echo "Laravel:  ${LARAVEL_CHAOS_PASSED} passed, ${LARAVEL_CHAOS_FAILED} failed"

TOTAL_FAILED=$((RUST_CHAOS_FAILED + LARAVEL_CHAOS_FAILED))

if [[ "${LAB_UP}" == false ]]; then
    echo ""
    echo "=== Stopping RabbitMQ lab ==="
    cd "${PROJECT_ROOT}"
    ./scripts/lab-down.sh
fi

if [[ ${TOTAL_FAILED} -gt 0 ]]; then
    echo ""
    echo "CHAOS TESTS FAILED: ${TOTAL_FAILED} scenario(s) failed"
    exit 1
fi

echo ""
echo "All chaos tests passed: missing = 0"
