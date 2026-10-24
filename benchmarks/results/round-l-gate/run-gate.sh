#!/usr/bin/env bash
# Round L gate (plan step F.3) — driver-bench standard protocol, non-regression re-bench.
#
# 5 cells × 3 interleaved passes, one JSON per run under raw/, mirroring the
# round-2 / round-i runner shape (scripts/rebench-driver-bench.sh) under the
# round-l-profile execution discipline:
#   - release cdylib loaded per-run (never installed system-wide),
#   - xdebug-free invocation (`php -n -d extension=<dylib>` — no ini files),
#   - classic durable queue (config patch documented in README.md, reverted
#     after the runs), delay.mode=ttl, prefetch 64, 1024 B Laravel envelope,
#   - fresh 3-node lab (lab-up.sh wipes volumes; lab-ready.sh passed).
#
# Usage: ./run-gate.sh   (must be run from this archive directory's checkout)
set -euo pipefail

ARCHIVE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${ARCHIVE}/../../.." && pwd)"
DYLIB="${ROOT}/target/release/librabbit_rs_php.dylib"
BENCH_DIR="${ROOT}/benchmarks/driver-bench"
RAW="${ARCHIVE}/raw"
COUNT=1000
ROUNDS=10
PASSES=3
LOADCSV="${ARCHIVE}/load-observations.csv"

[[ -x "${DYLIB}" ]] || { echo "error: release dylib missing: ${DYLIB}" >&2; exit 2; }

mkdir -p "${RAW}"
[[ -f "${LOADCSV}" ]] || echo "phase,timestamp,load1,load5,load15" > "${LOADCSV}"

load_sample() {
    # macOS load averages (1/5/15 min) as a CSV row
    local loads
    loads="$(sysctl -n vm.loadavg | awk '{print $2", "$3", "$4}' | tr -d ' ')"
    echo "$1,$(date +%H:%M:%S),${loads}" >> "${LOADCSV}"
}

report_cell() {
    local cell="$1" pass="$2"
    php -r '
        $d = json_decode(file_get_contents($argv[1]), true);
        printf(
            "  %-22s run%s: ok=%d losses=%d late=%d dup=%s reconnects=%s stalls=%s avg=%.0f ops/s\n",
            $argv[2], $argv[3],
            (int) $d["ok"], (int) $d["losses"], (int) $d["late_arrivals_after_drain"],
            var_export($d["duplicates"], true), var_export($d["reconnects_total"], true),
            var_export(array_sum(array_column($d["rounds_detail"], "stall_recoveries")), true),
            (float) $d["avg_rate_ops_s"]
        );
    ' "${RAW}/${cell}-run${pass}.json" "${cell}" "${pass}"
}

run_goopil() {
    local cell="$1" mode="$2" pass="$3" safety="$4"
    (
        cd "${BENCH_DIR}"
        if [[ -n "${safety}" ]]; then
            RABBIT_RS_SAFETY="${safety}" php -n -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
                --output="${RAW}/${cell}-run${pass}.json"
        else
            php -n -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
                --output="${RAW}/${cell}-run${pass}.json"
        fi
    ) > /dev/null 2> "${RAW}/${cell}-run${pass}.stderr.log"
    report_cell "${cell}" "${pass}"
}

run_vladimir() {
    local cell="$1" mode="$2" pass="$3"
    (
        cd "${BENCH_DIR}"
        php -n bin/bench.php \
            --connection=rabbitmq-amqplib --mode="${mode}" --count="${COUNT}" --rounds="${ROUNDS}" \
            --output="${RAW}/${cell}-run${pass}.json"
    ) > /dev/null 2> "${RAW}/${cell}-run${pass}.stderr.log"
    report_cell "${cell}" "${pass}"
}

for pass in $(seq 1 "${PASSES}"); do
    echo "=== pass ${pass}/${PASSES} $(date +%H:%M:%S) ==="
    load_sample "pass${pass}-start"
    run_goopil  goopil-dispatch-blind dispatch "${pass}" blind
    run_goopil  goopil-dispatch-safe  dispatch "${pass}" safe
    run_goopil  goopil-worker         worker   "${pass}" ""
    run_vladimir vladimir-dispatch    dispatch "${pass}"
    run_vladimir vladimir-worker      worker   "${pass}"
    load_sample "pass${pass}-end"
done

echo "=== gate runs complete; JSONs in ${RAW} ==="
