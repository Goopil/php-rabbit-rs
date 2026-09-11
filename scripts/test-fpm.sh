#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

ROOT_DIR="$(ext_project_root)"
FIXTURE_DIR="${ROOT_DIR}/crates/rabbit-rs-php/tests/fixtures/fpm"
PHP_BIN_PATH="$(command -v "${PHP_BIN:-php}")"
PHP_FPM_PATH="$(command -v "${PHP_FPM_BIN:-php-fpm}")"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/rabbit-rs-fpm.XXXXXX")"

ARTIFACT="$(ext_artifact_path)"

if [[ ! -f "${ARTIFACT}" ]]; then
    echo "extension artifact not found: ${ARTIFACT}" >&2
    echo "Build the extension first: cargo build --manifest-path ${ROOT_DIR}/crates/rabbit-rs-php/Cargo.toml --features extension-tests" >&2
    exit 1
fi

# Broker-backed certification needs the RabbitMQ lab (same cluster as
# test-integration.sh): the lone-publish and graceful-stop scenarios assert
# on real broker queue state, read through the AMQP data plane.
MGMT_USER="${RABBIT_RS_LAB_ADMIN_USER:-admin}"
MGMT_PASS="${RABBIT_RS_LAB_ADMIN_PASS:-admin_lab}"
# AMQP and management endpoints of the lab. Overridable for containerized CI
# runs where the lab lives on the docker host (host.docker.internal).
BROKER_HOST="${RABBIT_RS_FPM_BROKER_HOST:-127.0.0.1}"
BROKER_PORT="${RABBIT_RS_FPM_BROKER_PORT:-5672}"
MGMT_BASE="${RABBIT_RS_FPM_MGMT_BASE:-http://localhost:15672}"
export RABBIT_RS_BROKER_HOST="${BROKER_HOST}"
export RABBIT_RS_BROKER_PORT="${BROKER_PORT}"
# Set when the lab is already managed by the caller (CI starts it in its own
# step so the container itself does not need docker-in-docker).
EXTERNAL_LAB="${RABBIT_RS_FPM_EXTERNAL_LAB:-0}"
LAB_STARTED=false

export RABBIT_RS_FPM_PID="${TEST_DIR}/php-fpm.pid"
export RABBIT_RS_FPM_LOG="${TEST_DIR}/php-fpm.log"
export RABBIT_RS_FPM_SOCKET="${TEST_DIR}/php-fpm.sock"
if [[ "${EUID}" -eq 0 ]]; then
    export RABBIT_RS_FPM_USER="nobody"
    export RABBIT_RS_FPM_GROUP="$(id -gn nobody)"
    # The workers run as another user and write marker files into TEST_DIR.
    chmod 0777 "${TEST_DIR}"
else
    export RABBIT_RS_FPM_USER="$(id -un)"
    export RABBIT_RS_FPM_GROUP="$(id -gn)"
fi

FPM_PID=""
cleanup() {
    if [[ -n "${FPM_PID}" ]]; then
        kill -TERM "${FPM_PID}" 2>/dev/null || true
        wait "${FPM_PID}" 2>/dev/null || true
    fi
    if [[ "${LAB_STARTED}" == true ]]; then
        echo ""
        echo "=== Stopping RabbitMQ lab ==="
        (cd "${ROOT_DIR}" && ./scripts/lab-down.sh) || true
    fi
    rm -rf "${TEST_DIR}"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

start_fpm() {
    "${PHP_FPM_PATH}" -F -y "${FIXTURE_DIR}/php-fpm.conf" -d "extension=${ARTIFACT}" &
    FPM_PID=$!
    for _ in {1..100}; do
        if [[ -S "${RABBIT_RS_FPM_SOCKET}" ]]; then
            return 0
        fi
        if ! kill -0 "${FPM_PID}" 2>/dev/null; then
            echo "php-fpm stopped before creating its socket" >&2
            exit 1
        fi
        sleep 0.05
    done
    echo "php-fpm socket was not created" >&2
    exit 1
}

declare_queue() {
    curl -sf -u "${MGMT_USER}:${MGMT_PASS}" -X PUT -H 'content-type: application/json' \
        -d '{"durable": false}' \
        "${MGMT_BASE}/api/queues/%2F/$1" >/dev/null
}

# Queue depth is read through the AMQP data plane (a passive declare via the
# size.php observer), not the management API: the management endpoint may
# omit the `messages` gauge entirely until its stats collector emits one, so
# a fresh lab can make deadline polling read 0 forever (observed on live
# queues holding messages). The observer runs as its own CLI process with
# its own pool, so it never touches the publish path under test.
queue_depth() {
    local body
    if ! body="$("${PHP_BIN_PATH}" -d "extension=${ARTIFACT}" \
        "${FIXTURE_DIR}/size.php" "$1" 2>/dev/null)"; then
        echo 0
        return
    fi
    jq -r '.depth // 0' <<<"${body}" 2>/dev/null || echo 0
}

delete_queue() {
    curl -sf -u "${MGMT_USER}:${MGMT_PASS}" -X DELETE \
        "${MGMT_BASE}/api/queues/%2F/$1" >/dev/null 2>&1 || true
}

# Waits until the publish request wrote its marker file (the publication was
# accepted) or the request exited. $1 = marker path, $2 = request pid.
wait_for_marker() {
    for _ in {1..100}; do
        if [[ -f "$1" ]]; then
            return 0
        fi
        if ! kill -0 "$2" 2>/dev/null; then
            return 1
        fi
        sleep 0.05
    done
    return 1
}

# Pool-isolation certification: 16 concurrent requests across the 2 FPM
# workers must reuse one handle per worker and never share handles between
# workers. Run again after a SIGUSR2 reload (scenario D).
run_isolation_block() {
    "${PHP_BIN_PATH}" -- "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/index.php" "${FIXTURE_DIR}" <<'PHP'
<?php
declare(strict_types=1);

require $argv[3] . '/fcgi_client.php';

$streams = [];
for ($index = 0; $index < 16; $index++) {
    $streams[] = fcgi_begin_request($argv[1], $argv[2]);
}

$workers = [];
foreach ($streams as $stream) {
    $response = fcgi_finish_request($stream);
    if ($response['first_handle'] !== $response['second_handle']) {
        throw new RuntimeException('equivalent pools did not share a handle within one request');
    }
    $pid = (string) $response['pid'];
    if (isset($workers[$pid]) && $workers[$pid]['handle'] !== $response['first_handle']) {
        throw new RuntimeException('worker did not reuse its handle between requests');
    }
    $workers[$pid] = [
        'handle' => $response['first_handle'],
        'count' => ($workers[$pid]['count'] ?? 0) + 1,
    ];
}

if (count($workers) !== 2) {
    throw new RuntimeException('expected responses from two FPM workers');
}
if (count(array_unique(array_column($workers, 'handle'))) !== 2) {
    throw new RuntimeException('FPM workers announced the same handle');
}
foreach ($workers as $worker) {
    if ($worker['count'] < 2) {
        throw new RuntimeException('each FPM worker must serve multiple requests');
    }
}

echo "OK\n";
PHP
}

echo "=== Starting RabbitMQ lab ==="
if [[ "${EXTERNAL_LAB}" == "1" ]]; then
    echo "Using externally managed lab (${MGMT_BASE})"
else
    if ! docker ps >/dev/null 2>&1; then
        echo "ERROR: docker daemon is not running" >&2
        exit 1
    fi
    LAB_STARTED=true
    (cd "${ROOT_DIR}" && ./scripts/lab-up.sh with-plugin)
fi

echo "=== Waiting for lab readiness ==="
if [[ "${EXTERNAL_LAB}" != "1" ]]; then
    for i in $(seq 1 120); do
        if (cd "${ROOT_DIR}" && ./scripts/lab-ready.sh >/dev/null 2>&1); then
            break
        fi
        echo "  waiting for lab... (${i}s)"
        sleep 1
    done

    if ! (cd "${ROOT_DIR}" && ./scripts/lab-ready.sh >/dev/null 2>&1); then
        echo "ERROR: lab is not ready after 120s" >&2
        exit 1
    fi
fi
echo "Lab is ready."

echo ""
echo "=== Starting php-fpm ==="
start_fpm

echo ""
echo "=== Pool isolation block ==="
run_isolation_block

echo ""
echo "=== Scenario B: lone publish reaches the broker via the age-flush timer (issue #218) ==="
QUEUE_LONE="bench.fpm-lone-$(date +%s)-$$"
declare_queue "${QUEUE_LONE}"
LONE_PAYLOAD="fpm-lone-publish-$$"
MARKER_LONE="${TEST_DIR}/lone-published.json"

"${PHP_BIN_PATH}" "${FIXTURE_DIR}/fpm_request.php" \
    "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/publish.php" \
    "RABBIT_RS_QUEUE=${QUEUE_LONE}" \
    "RABBIT_RS_FLUSH_INTERVAL_MS=1500" \
    "RABBIT_RS_HOLD_MS=5000" \
    "RABBIT_RS_PAYLOAD=${LONE_PAYLOAD}" \
    "RABBIT_RS_BROKER_HOST=${BROKER_HOST}" \
    "RABBIT_RS_BROKER_PORT=${BROKER_PORT}" \
    "RABBIT_RS_MARKER_FILE=${MARKER_LONE}" \
    > "${TEST_DIR}/lone-response.json" 2> "${TEST_DIR}/lone-error.log" &
LONE_REQ_PID=$!

if ! wait_for_marker "${MARKER_LONE}" "${LONE_REQ_PID}"; then
    fail "publish request did not report its lone publication: $(cat "${TEST_DIR}/lone-error.log" 2>/dev/null || true)"
fi

# The request still holds the pool with the lone publication buffered and
# performs no follow-up operation. Sleep past the 1500 ms flush interval,
# then poll briefly for the depth (bounded well inside the 5 s hold so the
# destructor flush can never race in): because the request has not ended,
# only the background age-flush timer can have delivered the publication.
sleep 2
DEPTH=0
for _ in $(seq 1 8); do
    kill -0 "${LONE_REQ_PID}" 2>/dev/null || fail "publish request ended before the depth check"
    DEPTH="$(queue_depth "${QUEUE_LONE}")"
    if [[ "${DEPTH}" == "1" ]]; then
        break
    fi
    sleep 0.25
done
if [[ "${DEPTH}" != "1" ]]; then
    fail "expected queue depth 1 after the flush interval; got ${DEPTH}"
fi
echo "ok: lone publish became visible on the broker via the age-flush timer"

if ! wait "${LONE_REQ_PID}"; then
    fail "publish request failed: $(cat "${TEST_DIR}/lone-error.log" 2>/dev/null || true)"
fi

# End-to-end: the delivered message is consumable and identifiable.
CONSUMED="$("${PHP_BIN_PATH}" "${FIXTURE_DIR}/fpm_request.php" \
    "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/consume.php" \
    "RABBIT_RS_QUEUE=${QUEUE_LONE}" \
    "RABBIT_RS_BROKER_HOST=${BROKER_HOST}" \
    "RABBIT_RS_BROKER_PORT=${BROKER_PORT}")"
PUBLISHED_ID="$(jq -r '.message_id' "${TEST_DIR}/lone-response.json")"
if [[ "$(jq -r '.payload' <<< "${CONSUMED}")" != "${LONE_PAYLOAD}" ]]; then
    fail "consumed payload mismatch: ${CONSUMED}"
fi
if [[ "$(jq -r '.message_id' <<< "${CONSUMED}")" != "${PUBLISHED_ID}" ]]; then
    fail "consumed message_id mismatch: published ${PUBLISHED_ID}, consumed ${CONSUMED}"
fi
echo "ok: lone publish round-trips through the broker with its message id intact"
delete_queue "${QUEUE_LONE}"

echo ""
echo "=== Scenario D: graceful reload (SIGUSR2) survives with isolation intact ==="
kill -USR2 "${FPM_PID}"
# The master replaces its workers asynchronously; wait until a worker answers
# again and give the replaced workers a moment to exit before re-certifying
# isolation against exactly the reloaded pool.
for _ in $(seq 1 100); do
    if "${PHP_BIN_PATH}" "${FIXTURE_DIR}/fpm_request.php" \
        "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/index.php" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
sleep 1
run_isolation_block
echo "ok: pool isolation holds after a SIGUSR2 reload"

# Scenario C stops php-fpm by design, so it must run last.
echo ""
echo "=== Scenario C: graceful stop flushes the lone publication ==="
QUEUE_TERM="bench.fpm-term-$(date +%s)-$$"
declare_queue "${QUEUE_TERM}"
TERM_PAYLOAD="fpm-graceful-stop-$$"
MARKER_TERM="${TEST_DIR}/term-published.json"

# The age-flush interval is pushed to the 1 h validation ceiling so the
# background timer from scenario B can never fire within this scenario: only
# the stop-initiated teardown flush can deliver the publication.
"${PHP_BIN_PATH}" "${FIXTURE_DIR}/fpm_request.php" \
    "${RABBIT_RS_FPM_SOCKET}" "${FIXTURE_DIR}/publish.php" \
    "RABBIT_RS_QUEUE=${QUEUE_TERM}" \
    "RABBIT_RS_FLUSH_INTERVAL_MS=3600000" \
    "RABBIT_RS_HOLD_MS=5000" \
    "RABBIT_RS_PAYLOAD=${TERM_PAYLOAD}" \
    "RABBIT_RS_MARKER_FILE=${MARKER_TERM}" \
    > "${TEST_DIR}/term-response.json" 2> "${TEST_DIR}/term-error.log" &
TERM_REQ_PID=$!

if ! wait_for_marker "${MARKER_TERM}" "${TERM_REQ_PID}"; then
    fail "graceful-stop publish request did not report its publication: $(cat "${TEST_DIR}/term-error.log" 2>/dev/null || true)"
fi

# The request is still in flight (5 s hold, process_control_timeout = 10 s):
# stop php-fpm now. SIGQUIT is php-fpm's graceful-stop signal (SIGTERM is
# the immediate-termination signal and hard-kills in-flight requests): the
# worker finishes the request, the pool destructor flushes the
# still-buffered publication during request shutdown, and only then does
# the worker exit.
kill -QUIT "${FPM_PID}"
FPM_PID=""

TERM_DEPTH=0
for _ in $(seq 1 20); do
    TERM_DEPTH="$(queue_depth "${QUEUE_TERM}")"
    if [[ "${TERM_DEPTH}" == "1" ]]; then
        break
    fi
    sleep 0.5
done
if [[ "${TERM_DEPTH}" != "1" ]]; then
    fail "expected queue depth 1 within 10s after graceful stop; got ${TERM_DEPTH}"
fi
echo "ok: graceful stop flushed the lone publication to the broker"
delete_queue "${QUEUE_TERM}"

wait "${TERM_REQ_PID}" 2>/dev/null || true

echo ""
echo "FPM certification complete."
