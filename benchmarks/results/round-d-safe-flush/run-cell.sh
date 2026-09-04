#!/usr/bin/env bash
# Round D scratch runner: one driver-bench cell, 3 passes x 10 rounds.
# Usage: run-cell.sh <cell-safety|-> <mode> <outdir> [extra env as K=V ...]
set -euo pipefail
ROOT="/Users/zacharyvolpi/dev/perso/rabbit-rs/.worktrees/task/41-round-d"
DYLIB="${ROOT}/target/release/librabbit_rs_php.dylib"
BENCH_DIR="${ROOT}/benchmarks/driver-bench"
SAFETY="${1:?safety or -}"; MODE="${2:?mode}"; OUT="${3:?outdir}"; shift 3
mkdir -p "${OUT}"
OUT="$(cd "${OUT}" && pwd)"
cell="goopil-${MODE}"
[[ "${SAFETY}" != "-" ]] && cell="${cell}-${SAFETY}"
for pass in 1 2 3; do
    (
        cd "${BENCH_DIR}"
        if [[ "${SAFETY}" != "-" ]]; then
            RABBIT_RS_SAFETY="${SAFETY}" "$@" php -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${MODE}" --count=1000 --rounds=10 \
                --output="${OUT}/${cell}-run${pass}.json"
        else
            "$@" php -d "extension=${DYLIB}" bin/bench.php \
                --connection=rabbit-rs --mode="${MODE}" --count=1000 --rounds=10 \
                --output="${OUT}/${cell}-run${pass}.json"
        fi
    ) >/dev/null 2>&1
    php -r '$d=json_decode(file_get_contents($argv[1]),true); printf("%s run%d: ok=%d rate=%.0f\n", $argv[1] ? basename($argv[1], ".json") : "?", (int)$argv[2], $d["ok"], $d["avg_rate_ops_s"]);' "${OUT}/${cell}-run${pass}.json" "${pass}"
done
