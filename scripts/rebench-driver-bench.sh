#!/usr/bin/env bash
# Round 2 re-bench runner — driver-bench cells, interleaved per the release protocol.
# Usage: ./scripts/rebench-driver-bench.sh <archive-dir>
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ARCHIVE="${1:?usage: rebench-driver-bench.sh <archive-dir>}"
mkdir -p "${ARCHIVE}"
ARCHIVE="$(cd "${ARCHIVE}" && pwd)"

DYLIB="${ROOT}/target/release/librabbit_rs_php.dylib"
BENCH_DIR="${ROOT}/benchmarks/driver-bench"
COUNT=1000
ROUNDS=10
PASSES=3

run_goopil() {
    local cell="$1" mode="$2" pass="$3" safety="$4"
    (
        cd "${BENCH_DIR}"
        if [[ -n "${safety}" ]]; then
            RABBIT_RS_SAFETY="${safety}" php -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
                --output="${ARCHIVE}/${cell}-run${pass}.json"
        else
            php -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
                --output="${ARCHIVE}/${cell}-run${pass}.json"
        fi
    ) >/dev/null 2>&1
    local ok losses late
    ok="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["ok"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    losses="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["losses"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    late="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["late_arrivals_after_drain"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    echo "done ${cell} run${pass}: ok=${ok} losses=${losses} late=${late}"
}

run_vladimir() {
    local cell="$1" mode="$2" pass="$3"
    (
        cd "${BENCH_DIR}"
        php bin/bench.php \
            --connection=rabbitmq-amqplib --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
            --output="${ARCHIVE}/${cell}-run${pass}.json"
    ) >/dev/null 2>&1
    local ok losses late
    ok="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["ok"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    losses="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["losses"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    late="$(php -r '$d=json_decode(file_get_contents($argv[1]),true); echo (int)$d["late_arrivals_after_drain"];' "${ARCHIVE}/${cell}-run${pass}.json")"
    echo "done ${cell} run${pass}: ok=${ok} losses=${losses} late=${late}"
}

for pass in $(seq 1 "${PASSES}"); do
    echo "=== pass ${pass}/${PASSES} ==="
    run_goopil  goopil-dispatch-blind dispatch "${pass}" blind
    run_goopil  goopil-dispatch-safe  dispatch "${pass}" safe
    run_goopil  goopil-worker         worker   "${pass}" ""
    run_vladimir vladimir-dispatch    dispatch "${pass}"
    run_vladimir vladimir-worker      worker   "${pass}"
done

echo "=== all driver-bench runs archived to ${ARCHIVE} ==="
