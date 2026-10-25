#!/usr/bin/env bash
set -euo pipefail

# Issue #368 — one-command reproduction of the public driver comparison.
#
#   ./benchmarks/run-comparison.sh [--output-dir=results/<dir>] [--rounds=5]
#
# Orchestrates one invocation of src/run-comparison.php per (scenario, driver)
# pair with the PHP binary that owns it (ext-rabbit_rs is built against 8.5,
# pecl amqp ships for 8.4 locally), interleaved per cycle, then merges the
# partials into <output-dir>/comparison-results.json.

cd "$(dirname "$0")"

OUT_DIR="${OUT_DIR:-results/comparison-run}"
ROUNDS="${ROUNDS:-5}"
KILL_CONTAINER="${KILL_CONTAINER:-rabbitrs-rabbitmq-1-1}"
PHP85="${PHP85:-$(command -v php)}"
PHP84="${PHP84:-/opt/homebrew/opt/php@8.4/bin/php}"
DYLIB="${RABBIT_RS_DYLIB:-../target/release/librabbit_rs_php.dylib}"
REPO_ROOT="$(cd .. && pwd)"

if ! curl -sf -o /dev/null -u rabbit_rs:rabbit_rs_lab http://localhost:15672/api/overview; then
    echo "RabbitMQ lab not reachable on 127.0.0.1:5672 — start it with: ./scripts/lab-up.sh with-plugin" >&2
    exit 1
fi

if [[ ! -d vendor ]]; then
    composer install --no-interaction
fi

if [[ ! -f "${DYLIB}" ]]; then
    echo "release extension not found — building (one-time, slow)..."
    cargo build --release --manifest-path "${REPO_ROOT}/crates/rabbit-rs-php/Cargo.toml"
fi

if [[ ! -x "${PHP84}" ]]; then
    PHP84="$(command -v php8.4 || true)"
fi
if [[ -z "${PHP84}" || ! -x "${PHP84}" ]]; then
    echo "PHP 8.4 binary with pecl amqp not found (set PHP84=...) — the amqp-ext driver cannot run" >&2
    exit 1
fi

mkdir -p "${OUT_DIR}"
rm -f "${OUT_DIR}"/partial-*.json

# Throughput scenarios: per cycle, the three drivers alternate A/B/A/B
# (round-level interleaving is impossible across PHP binaries; the cycle
# keeps the drift-sharing alternation of the release protocol).
warmup=1
for cycle in $(seq 1 "${ROUNDS}"); do
    echo ""
    echo "### cycle ${cycle}/${ROUNDS} ###"
    for scenario in publish-fanout sustained-consume laravel-worker; do
        "${PHP85}" -d xdebug.mode=off -d extension="${DYLIB}" src/run-comparison.php \
            --driver=rabbit-rs --scenario="${scenario}" --rounds=1 --warmup="${warmup}" \
            --kill-container="${KILL_CONTAINER}" --output-dir="${OUT_DIR}"
        "${PHP85}" -d xdebug.mode=off src/run-comparison.php \
            --driver=amqplib --scenario="${scenario}" --rounds=1 --warmup="${warmup}" \
            --kill-container="${KILL_CONTAINER}" --output-dir="${OUT_DIR}"
        "${PHP84}" -d xdebug.mode=off src/run-comparison.php \
            --driver=amqp-ext --scenario="${scenario}" --rounds=1 --warmup="${warmup}" \
            --kill-container="${KILL_CONTAINER}" --output-dir="${OUT_DIR}"
    done
    warmup=0
done

# Broker-kill: one full-restart-under-load run per driver.
for driver in rabbit-rs amqplib amqp-ext; do
    php_cmd=("${PHP85}" -d xdebug.mode=off)
    [[ "${driver}" == "rabbit-rs" ]] && php_cmd=("${PHP85}" -d xdebug.mode=off -d extension="${DYLIB}")
    [[ "${driver}" == "amqp-ext" ]] && php_cmd=("${PHP84}" -d xdebug.mode=off)
    "${php_cmd[@]}" src/run-comparison.php \
        --driver="${driver}" --scenario=broker-kill \
        --kill-container="${KILL_CONTAINER}" --output-dir="${OUT_DIR}"
done

# Merge the partials into the published envelope (integrity gates inside).
"${PHP85}" -d xdebug.mode=off src/run-comparison.php --merge --output-dir="${OUT_DIR}"
