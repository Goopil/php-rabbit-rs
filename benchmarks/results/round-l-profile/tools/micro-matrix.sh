#!/usr/bin/env bash
# Round L profile — micro matrix runner (xdebug-free, per-run load provenance).
# Usage: micro-matrix.sh   (writes profiles/*.json + profiles/load-observations.csv)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
DYLIB="${ROOT}/target/release/librabbit_rs_php.dylib"
OUT="${ROOT}/benchmarks/results/round-l-profile/profiles"
TOOLS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CSV="${OUT}/load-observations.csv"
export RABBIT_RS_DYLIB="${DYLIB}"

load_now() { uptime | sed -E 's/.*load averages: //' | tr ',' ' '; }
observe() { # observe <label>
  echo "$(date +%H:%M:%S),${1},$(load_now)" >> "${CSV}"
}

run_publish() { # run_publish <safety>
  observe "publish-${1}-start"
  RABBIT_RS_SAFETY="${1}" php -n -d extension="${DYLIB}" "${TOOLS}/micro-publish.php" \
    --iters=50000 --mode=single > "${OUT}/publish-${1}-50k.json"
  observe "publish-${1}-end"
  echo "publish-${1}: $(python3 -c "import json;d=json.load(open('${OUT}/publish-${1}-50k.json'));print(d['rate_ops_s'],'ops/s p50',d['per_op_us']['p50'],'us')")"
}

run_consume() { # run_consume <name> <extra args...>
  local name="${1}"; shift
  observe "consume-${name}-start"
  php -n -d extension="${DYLIB}" "${TOOLS}/micro-consume.php" "$@" > "${OUT}/consume-${name}.json"
  observe "consume-${name}-end"
  echo "consume-${name}: $(python3 -c "import json;d=json.load(open('${OUT}/consume-${name}.json'));print(json.dumps(d['stats_us']))")"
}

echo "timestamp,cell,load1,load5,load15" > "${CSV}"
observe "session-start"
uptime

run_publish safe
run_publish blind
run_publish unsafe

run_consume trynull --stage=trynull --iters=200000
run_consume trynull-run2 --stage=trynull --iters=200000
run_consume null --stage=null --iters=50000
run_consume null-run2 --stage=null --iters=50000

for r in "" -run2 -run3; do
  run_consume "hot-fill1000${r}" --stage=hot --fill=1000
done
run_consume hot-fill1000-prefetch500 --stage=hot --fill=1000 --iters=1000
run_consume earlyack-fill5000 --stage=earlyack --fill=5000
run_consume batch-fill5000 --stage=batch --fill=5000

observe "session-end"
echo "--- load observations ---"
cat "${CSV}"
