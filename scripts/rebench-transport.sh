#!/usr/bin/env bash
# Round 2 re-bench runner — transport-level harness cells (benchmarks/src), interleaved.
# Usage: ./scripts/rebench-transport.sh <archive-dir>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ARCHIVE="${1:?usage: rebench-transport.sh <archive-dir>}"
mkdir -p "${ARCHIVE}"
ARCHIVE="$(cd "${ARCHIVE}" && pwd)"

DYLIB="${ROOT}/target/release/librabbit_rs_php.dylib"
RUNNER="${ROOT}/benchmarks/src/run-benchmarks.php"
RESULTS="${ROOT}/benchmarks/results/benchmark-results.json"
PASSES=3
SCENARIOS=(fire-and-forget batch-confirm laravel-dispatch laravel-worker)

run_cell() {
    local scenario="$1" driver="$2" pass="$3"
    local log="${ARCHIVE}/transport-${scenario}-${driver}-run${pass}.log"
    if [[ "${driver}" == "rabbit-rs" ]]; then
        php -d "extension=${DYLIB}" "${RUNNER}" --scenario="${scenario}" --driver="${driver}" \
            > "${log}" 2>&1
    else
        php "${RUNNER}" --scenario="${scenario}" --driver="${driver}" \
            > "${log}" 2>&1
    fi
    cp "${RESULTS}" "${ARCHIVE}/transport-${scenario}-${driver}-run${pass}.json"
    local summary
    summary="$(rg -o "${scenario}/${driver}\s+\|.*" "${log}" | head -1 || true)"
    echo "done ${scenario}/${driver} run${pass}: ${summary}"
}

for pass in $(seq 1 "${PASSES}"); do
    echo "=== pass ${pass}/${PASSES} ==="
    for scenario in "${SCENARIOS[@]}"; do
        run_cell "${scenario}" rabbit-rs "${pass}"
        run_cell "${scenario}" amqplib "${pass}"
    done
done

echo "=== all transport runs archived to ${ARCHIVE} ==="
