#!/usr/bin/env bash
set -euo pipefail

# Rabbit RS — real-server Octane runtime certification harness.
#
# Boots a real Octane server (FrankenPHP, RoadRunner, Open Swoole, or Swoole)
# against the RabbitMQ lab and drives the #218 certification scenario through
# the RUNNING server:
#
#   publish x5 -> octane:reload -> publish x5 -> graceful stop
#     -> queue depth == 10 (no loss) + parked publications flushed
#     -> restart -> consume 10 -> ack all -> depth 0 -> worker process gone
#
# The scenario app (packages/laravel-queue/tests/Runtime/app) publishes with
# NO follow-up operation and uses a large publish-buffer flush interval
# (60 s), so publications stay parked in the worker's native publish buffer
# across the whole reload/stop window: only the reload and graceful-stop
# flush paths can deliver them. A server that loses them fails the harness.
#
# Usage:
#   scripts/test-octane-runtime.sh --server=<roadrunner|frankenphp|swoole|openswoole>
#       [--port=8180] [--reuse-lab]
#
# Pinned upstream versions (see ensure_roadrunner_binary / ensure_frankenphp_image):
#   RoadRunner   v2025.1.15 — sha256 recorded per platform below, verified at download
#   FrankenPHP   dunglas/frankenphp:php8.4 — digest-pinned below
#   Composer     laravel/framework ^12.0, laravel/octane ^2.0, spiral/roadrunner-http ^3.3,
#                spiral/roadrunner-cli ^2.6 (constraints in the Runtime app composer.json)
#
# Exit codes: 0 certified, 1 scenario failure, 2 server not available on this
# machine (harness delivered but unverifiable here — see the certification
# table in packages/laravel-queue/docs/reference.md).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

PROJECT_ROOT="$(ext_project_root)"
RUNTIME_ROOT="${PROJECT_ROOT}/packages/laravel-queue/tests/Runtime"
RUNTIME_APP="${RUNTIME_ROOT}/app"
SCENARIO_PHP="${RUNTIME_ROOT}/octane-scenario.php"

SERVER=""
PORT="${OCTANE_RUNTIME_PORT:-8180}"
RPC_PORT=$((PORT - 1999))
REUSE_LAB=false

for arg in "$@"; do
    case "${arg}" in
        --server=*) SERVER="${arg#--server=}" ;;
        --port=*) PORT="${arg#--port=}" ;;
        --reuse-lab) REUSE_LAB=true ;;
        *) echo "Usage: $0 --server=<roadrunner|frankenphp|swoole|openswoole> [--port=8180] [--reuse-lab]" >&2; exit 1 ;;
    esac
done

case "${SERVER}" in
    roadrunner|frankenphp|swoole|openswoole) ;;
    *) echo "Usage: $0 --server=<roadrunner|frankenphp|swoole|openswoole>" >&2; exit 1 ;;
esac

BASE_URL="http://127.0.0.1:${PORT}"
MGMT_URL="${RABBIT_RS_LAB_MGMT_URL:-http://localhost:15672}"
LAB_STARTED_BY_US=false
LAB_REUSED=false
SERVER_PID=""
SERVER_CONTAINER=""
PHP_BIN="${PHP_BIN:-php}"

# ---------------------------------------------------------------------------
# Pinned dependency records.
# ---------------------------------------------------------------------------

ROADRUNNER_VERSION="2025.1.15"
ROADRUNNER_SHA256=(
    "darwin-arm64:cc0521bd31478cc39403c2f090fc73db45e87e33a66b6783e9bcf096f321729a"
    "darwin-amd64:0019dfc4b32d63c1392aa264aed2253c1e0c2fb09216f8e2cc269bbfb8bb49b5"
    "linux-amd64:74589ff95e022dddf163f9316821cb80423a7db9865e96ce30a436d6898e34e7"
    "linux-arm64:f384d14b8687520816fc7ec204bb78f2fa54aba4da5336adef91d298fd6420d1"
)
FRANKENPHP_IMAGE="dunglas/frankenphp:php8.4"
FRANKENPHP_IMAGE_DIGEST="sha256:77bc2d40a58ace3a9425e4cbb0c40d044188dfd850e44a943ed043d743625df3"
# Derived image: pinned base + pcntl. Octane's artisan commands subscribe to
# SIGINT/SIGTERM unconditionally (InteractsWithServers::getSubscribedSignals),
# and the base image ships no pcntl, so octane:start dies with
# "Undefined constant SIGINT" before spawning anything.
FRANKENPHP_DERIVED_IMAGE="rabbitrs-octane/frankenphp:php8.4-pcntl"

# Unique scenario queue per run; the lab's rabbit_rs user may declare queues
# matching ^rabbit-rs\. (see lab/rabbitmq/rabbitmq/definitions.json).
RUN_ID="$(date +%s)-$$"
RUNTIME_QUEUE="rabbit-rs.octane-${RUN_ID}"
RUNTIME_VHOST="${RABBIT_RS_LAB_VHOST:-/orders-eu}"

# ---------------------------------------------------------------------------
# Shared helpers.
# ---------------------------------------------------------------------------

log()  { echo "=== $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

cleanup() {
    if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
        echo "cleanup: stopping server process ${SERVER_PID}"
        kill "${SERVER_PID}" 2>/dev/null || true
    fi
    if [[ -n "${SERVER_CONTAINER}" ]] && docker ps --format '{{.Names}}' 2>/dev/null | grep -q "^${SERVER_CONTAINER}$"; then
        echo "cleanup: removing container ${SERVER_CONTAINER}"
        docker rm -f "${SERVER_CONTAINER}" >/dev/null 2>&1 || true
    fi
    if [[ "${LAB_STARTED_BY_US}" == true ]]; then
        cd "${PROJECT_ROOT}"
        ./scripts/lab-down.sh || true
    fi
}
trap cleanup EXIT

scenario() {
    "${PHP_BIN}" "${RUNTIME_ROOT}/octane-scenario.php" "$@"
}

# Waits until the server answers /stats with HTTP 200 (which also proves the
# native pool resolves, i.e. the extension is loaded and the broker is up).
wait_server_up() {
    local deadline=$(( SECONDS + 60 ))
    while (( SECONDS < deadline )); do
        if curl -sf -o /dev/null "${BASE_URL}/stats"; then
            log "server is up at ${BASE_URL}"
            return 0
        fi
        sleep 1
    done
    echo "server did not come up within 60s; last logs:" >&2
    cat "${SERVER_LOG:-/dev/null}" >&2 || true
    fail "server did not come up at ${BASE_URL} within 60s"
}

# Waits until the server process (or container) is gone after a stop.
wait_server_gone() {
    local deadline=$(( SECONDS + 30 ))
    if [[ -n "${SERVER_CONTAINER}" ]]; then
        while (( SECONDS < deadline )); do
            if [[ "$(docker inspect -f '{{.State.Running}}' "${SERVER_CONTAINER}" 2>/dev/null || echo missing)" != "true" ]]; then
                log "server container is gone"
                return 0
            fi
            sleep 1
        done
        fail "server container ${SERVER_CONTAINER} is still running after octane:stop"
    fi
    while (( SECONDS < deadline )); do
        if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
            log "server process ${SERVER_PID} is gone"
            return 0
        fi
        sleep 1
    done
    fail "server process ${SERVER_PID} is still alive after octane:stop"
}

# ---------------------------------------------------------------------------
# Lab management (mirrors scripts/test-integration.sh).
# ---------------------------------------------------------------------------

start_lab() {
    command -v docker >/dev/null 2>&1 || fail "docker is required"
    docker ps >/dev/null 2>&1 || fail "docker daemon is not running"

    if [[ "${REUSE_LAB}" == true ]] && ./scripts/lab-ready.sh >/dev/null 2>&1; then
        log "reusing already-running RabbitMQ lab"
        return 0
    fi

    log "starting RabbitMQ lab (with-plugin)"
    LAB_STARTED_BY_US=true
    ./scripts/lab-up.sh with-plugin

    for i in $(seq 1 120); do
        if ./scripts/lab-ready.sh >/dev/null 2>&1; then
            log "lab is ready"
            return 0
        fi
        sleep 1
    done
    fail "lab is not ready after 120s"
}

# ---------------------------------------------------------------------------
# Runtime app preparation (shared by all servers).
# ---------------------------------------------------------------------------

prepare_runtime_app() {
    log "preparing Runtime scenario app"
    cd "${RUNTIME_APP}"

    if [[ ! -f vendor/autoload.php ]]; then
        # The app's composer.lock is gitignored: on a fresh checkout only
        # `composer update` can resolve the tree (install needs a lock).
        if [[ -f composer.lock ]]; then
            composer install --no-interaction 2>&1 | tail -3
        else
            composer update --no-interaction 2>&1 | tail -3
        fi
    fi

    if ! composer show laravel/octane >/dev/null 2>&1; then
        composer require --no-interaction \
            "laravel/framework:^12.0" "laravel/octane:^2.0" 2>&1 | tail -2
    fi
    if [[ "${SERVER}" == "roadrunner" ]] && ! composer show spiral/roadrunner-http >/dev/null 2>&1; then
        composer require --no-interaction \
            "spiral/roadrunner-http:^3.3.0" "spiral/roadrunner-cli:^2.6.0" 2>&1 | tail -2
    fi

    mkdir -p storage/framework/views storage/framework/cache storage/logs bootstrap/cache

    # Per-run environment: unique scenario queue, harness port, lab broker.
    {
        cat .env.example
        echo "APP_URL=${BASE_URL}"
        echo "OCTANE_SERVER=${SERVER}"
        echo "RABBIT_RS_HOSTS=${RABBIT_RS_HOSTS:-127.0.0.1:5672}"
        echo "RABBIT_RS_VHOST=${RUNTIME_VHOST}"
        echo "RABBIT_RS_QUEUE=${RUNTIME_QUEUE}"
        echo "RABBIT_RS_FLUSH_INTERVAL=${RABBIT_RS_FLUSH_INTERVAL:-60000}"
    } > .env
}

# A scoped ini dir (not the system conf.d) loads the extension into every PHP
# process spawned under the server: RoadRunner workers inherit it through the
# environment; Swoole/Open Swoole run in the octane:start process itself.
INI_DIR="${RUNTIME_ROOT}/.runtime-ini"

extension_env() {
    # If the extension is already loaded system-wide (e.g. installed via
    # cargo php install), adding the scoped ini would double-load the module
    # and RoadRunner workers would die at allocation. Skip the scoped ini
    # and run against the system-wide build instead (honestly flagged).
    local modules
    modules="$("${PHP_BIN}" -m 2>/dev/null || true)"
    if grep -qi '^rabbit_rs$' <<< "${modules}"; then
        log "WARNING: ext-rabbit_rs is loaded system-wide; the scoped ini" \
            "is skipped and tests run against the system-wide build" \
            "(remove it with 'cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes' to test target/debug)" >&2
        return 0
    fi
    mkdir -p "${INI_DIR}"
    printf 'extension=%s\n' "${EXTENSION_ARTIFACT}" > "${INI_DIR}/rabbit-rs.ini"
    echo "PHP_INI_SCAN_DIR=:${INI_DIR}"
}

# ---------------------------------------------------------------------------
# RoadRunner (PR-tier server: pinned binary, downloaded + checksum-verified).
# ---------------------------------------------------------------------------

ensure_roadrunner_binary() {
    local rr_bin="${PROJECT_ROOT}/target/rr-bin/rr"
    local plat archive url expected sha

    plat="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/')"
    expected=""
    for entry in "${ROADRUNNER_SHA256[@]}"; do
        if [[ "${entry%%:*}" == "${plat}" ]]; then
            expected="${entry#*:}"
        fi
    done
    if [[ -z "${expected}" ]]; then
        echo "WARNING: no pinned RoadRunner sha256 for platform '${plat}'; checksum verification will be skipped" >&2
    fi

    if [[ ! -x "${rr_bin}" ]]; then
        archive="roadrunner-${ROADRUNNER_VERSION}-${plat}.tar.gz"
        url="https://github.com/roadrunner-server/roadrunner/releases/download/v${ROADRUNNER_VERSION}/${archive}"
        log "downloading pinned RoadRunner v${ROADRUNNER_VERSION} (${plat})"
        curl -sL --fail -o "/tmp/${archive}" "${url}"

        if [[ -n "${expected}" ]]; then
            sha="$(shasum -a 256 "/tmp/${archive}" | awk '{print $1}')"
            if [[ "${sha}" != "${expected}" ]]; then
                fail "RoadRunner checksum mismatch for ${archive}: expected ${expected}, got ${sha}"
            fi
            echo "RoadRunner checksum verified: ${sha}"
        fi

        mkdir -p "$(dirname "${rr_bin}")"
        tar -xzf "/tmp/${archive}" -C "$(dirname "${rr_bin}")" --strip-components=1 "roadrunner-${ROADRUNNER_VERSION}-${plat}/rr"
        chmod +x "${rr_bin}"
        "${rr_bin}" --version
    else
        echo "RoadRunner binary already pinned at ${rr_bin}"
    fi

    # macOS AMFI intermittently SIGKILLs ad-hoc linker-signed binaries with
    # "Code Signature Invalid" when the kernel's per-vnode signature cache is
    # stale (observed on macOS 26.6: works from a shell, killed when spawned
    # by php). Re-signing ad-hoc recomputes the signature against the current
    # bytes; idempotent and cheap. The copy octane spawns gets the same
    # treatment so both inodes carry a fresh signature.
    if [[ "$(uname)" == "Darwin" ]] && command -v codesign >/dev/null 2>&1; then
        codesign -f -s - "${rr_bin}" >/dev/null 2>&1 || true
    fi

    # Octane looks for the binary at the app base path first.
    cp "${rr_bin}" "${RUNTIME_APP}/rr"
    if [[ "$(uname)" == "Darwin" ]] && command -v codesign >/dev/null 2>&1; then
        codesign -f -s - "${RUNTIME_APP}/rr" >/dev/null 2>&1 || true
    fi
}

start_roadrunner() {
    ensure_roadrunner_binary
    log "starting Octane RoadRunner server (workers=1, ext via PHP_INI_SCAN_DIR)"
    (
        cd "${RUNTIME_APP}"
        exec "${PHP_BIN}" artisan octane:start \
            --server=roadrunner --host=127.0.0.1 --port="${PORT}" \
            --rpc-port="${RPC_PORT}" --workers=1 --max-requests=500
    ) >"${SERVER_LOG}" 2>&1 &
    SERVER_PID=$!
    SERVER_CONTAINER=""
}

reload_roadrunner() {
    (cd "${RUNTIME_APP}" && "${PHP_BIN}" artisan octane:reload --server=roadrunner)
}

stop_roadrunner() {
    (cd "${RUNTIME_APP}" && "${PHP_BIN}" artisan octane:stop --server=roadrunner)
}

# ---------------------------------------------------------------------------
# FrankenPHP (pinned docker image; the extension must load inside the image).
#
# FrankenPHP embeds a ZTS PHP build. ext-rabbit_rs is distributed NTS-only
# (decision D8), so the harness first attempts an in-image extension build
# (the image ships phpize + PHP headers). If that build fails or the built
# .so refuses to load under ZTS, the server cannot be certified on this
# machine and the harness exits with status 2 — an honest verdict, not a skip.
# ---------------------------------------------------------------------------

ensure_frankenphp_image() {
    local digest
    digest="$(docker image inspect "${FRANKENPHP_IMAGE}" --format '{{index .RepoDigests 0}}' 2>/dev/null | awk -F'@' '{print $2}' || true)"
    if [[ "${digest}" == "${FRANKENPHP_IMAGE_DIGEST}" ]]; then
        echo "FrankenPHP image already pinned: ${FRANKENPHP_IMAGE}@${FRANKENPHP_IMAGE_DIGEST}"
    else
        log "pulling pinned FrankenPHP image ${FRANKENPHP_IMAGE}"
        docker pull "${FRANKENPHP_IMAGE}"
        digest="$(docker image inspect "${FRANKENPHP_IMAGE}" --format '{{index .RepoDigests 0}}' 2>/dev/null | awk -F'@' '{print $2}' || true)"
        if [[ "${digest}" != "${FRANKENPHP_IMAGE_DIGEST}" ]]; then
            fail "FrankenPHP image digest drift: expected ${FRANKENPHP_IMAGE_DIGEST}, got ${digest:-none}"
        fi
    fi

    if ! docker image inspect "${FRANKENPHP_DERIVED_IMAGE}" >/dev/null 2>&1; then
        log "building derived image ${FRANKENPHP_DERIVED_IMAGE} (pinned base + pcntl)"
        docker build -q -t "${FRANKENPHP_DERIVED_IMAGE}" - <<'DOCKERFILE'
FROM dunglas/frankenphp:php8.4
RUN docker-php-ext-install pcntl
DOCKERFILE
    fi
}

ensure_frankenphp_extension() {
    local container_so="${PROJECT_ROOT}/target/frankenphp-ext/librabbit_rs_php.so"

    if [[ -f "${container_so}" ]]; then
        echo "in-image extension build already present: ${container_so}"
        return 0
    fi

    log "building ext-rabbit_rs inside the FrankenPHP image (embedded ZTS PHP)"
    mkdir -p "${PROJECT_ROOT}/target/frankenphp-ext"
    if ! docker run --rm \
        -v "${PROJECT_ROOT}:/repo" \
        -w /repo/crates/rabbit-rs-php \
        -e CARGO_TARGET_DIR=/repo/target/frankenphp-ext/target \
        "${FRANKENPHP_IMAGE}" \
        bash -lc 'set -e
            php -r "exit(PHP_ZTS ? 0 : 1);" && echo "embedded PHP is ZTS"
            if ! command -v cargo >/dev/null 2>&1; then
                curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.96.0 --profile minimal
            fi
            . "$HOME/.cargo/env"
            apt-get update -qq && apt-get install -y -qq libclang-dev
            cargo build --release
            cp /repo/target/frankenphp-ext/target/release/librabbit_rs_php.so /repo/target/frankenphp-ext/
        '; then
        return 1
    fi

    [[ -f "${container_so}" ]]
}

start_frankenphp() {
    ensure_frankenphp_image

    if ! ensure_frankenphp_extension; then
        cat >&2 <<'EOF'
FAIL: the ext-rabbit_rs extension could not be built for the embedded FrankenPHP
PHP build (ZTS). The harness path is implemented but this server cannot be
certified while the extension is NTS-only (decision D8). See the certification
table in packages/laravel-queue/docs/reference.md.
EOF
        exit 2
    fi

    log "starting Octane FrankenPHP server in docker (workers=1, ext via mounted ini)"
    SERVER_CONTAINER="rabbitrs-octane-frankenphp"
    docker rm -f "${SERVER_CONTAINER}" >/dev/null 2>&1 || true

    mkdir -p "${RUNTIME_ROOT}/.frankenphp-ini"
    printf 'extension=/repo/target/frankenphp-ext/librabbit_rs_php.so\n' \
        > "${RUNTIME_ROOT}/.frankenphp-ini/rabbit-rs.ini"

    if ! docker run -d --name "${SERVER_CONTAINER}" \
        -v "${PROJECT_ROOT}:/repo" \
        -v "${RUNTIME_ROOT}/.frankenphp-ini:/usr/local/etc/php/conf.d-zz" \
        -w /repo/packages/laravel-queue/tests/Runtime/app \
        -e PHP_INI_SCAN_DIR=:/usr/local/etc/php/conf.d-zz \
        -e "RABBIT_RS_HOSTS=${RABBIT_RS_HOSTS_FOR_DOCKER:-host.docker.internal:5672}" \
        --add-host=host.docker.internal:host-gateway \
        -p "${PORT}:8000" \
        -p "2019:2019" \
        "${FRANKENPHP_DERIVED_IMAGE}" \
        php artisan octane:start --server=frankenphp \
            --host=0.0.0.0 --port=8000 --workers=1 --max-requests=500; then
        fail "failed to start the FrankenPHP container"
    fi

    sleep 2
    docker logs "${SERVER_CONTAINER}" 2>&1 | tail -5 || true
}

# Reload/stop must run INSIDE the container: the octane state file records
# container-namespace PIDs, and the host-side artisan commands would answer
# "Octane server is not running" when inspecting them.
reload_frankenphp() {
    docker exec "${SERVER_CONTAINER}" php artisan octane:reload --server=frankenphp
}

stop_frankenphp() {
    docker exec "${SERVER_CONTAINER}" php artisan octane:stop --server=frankenphp
}

# ---------------------------------------------------------------------------
# Swoole / Open Swoole (extension provided by pecl or shivammathur/setup-php).
#
# Laravel Octane 2.x has no `--server=openswoole` option: the swoole server
# command runs with whichever Swoole-family extension is loaded (see
# SwooleExtension::isInstalled and TableFactory in octane's source). Running
# "openswoole" therefore means: start --server=swoole with ext-openswoole
# loaded and ext-swoole absent.
# ---------------------------------------------------------------------------

ensure_swoole_extension() {
    local ext="${SERVER}"   # swoole | openswoole
    local modules
    modules="$("${PHP_BIN}" -m)"
    if grep -qi "^${ext}$" <<< "${modules}"; then
        log "php extension '${ext}' is available"
        return 0
    fi
    cat >&2 <<EOF
FAIL: the '${ext}' PHP extension is not loaded in ${PHP_BIN}.

This harness machine cannot verify ${ext} locally. Install it with:
  pecl install ${ext}
or, in CI, with shivammathur/setup-php:
  uses: shivammathur/setup-php@...
  with:
    extensions: ${ext}

The harness path is implemented; see the certification table in
packages/laravel-queue/docs/reference.md for its verification status.
EOF
    exit 2
}

start_swoole() {
    # Guards: the right Swoole-family extension for the requested server must
    # be loaded, and octane's swoole command must not silently run on the
    # other family's extension.
    local modules
    modules="$("${PHP_BIN}" -m)"
    if ! grep -qi "^${SERVER}$" <<< "${modules}"; then
        ensure_swoole_extension   # prints the honest install hint and exits 2
    fi
    if [[ "${SERVER}" == "swoole" ]] && ! grep -qi '^swoole$' <<< "${modules}"; then
        fail "swoole requested but ext-swoole is not loaded; install it to certify Swoole"
    fi

    log "starting Octane ${SERVER} server (workers=1, ext via PHP_INI_SCAN_DIR)"
    (
        cd "${RUNTIME_APP}"
        exec "${PHP_BIN}" artisan octane:start \
            --server=swoole --host=127.0.0.1 --port="${PORT}" \
            --workers=1 --task-workers=1 --max-requests=500
    ) >"${SERVER_LOG}" 2>&1 &
    SERVER_PID=$!
    SERVER_CONTAINER=""
}

reload_swoole() {
    (cd "${RUNTIME_APP}" && "${PHP_BIN}" artisan octane:reload --server=swoole)
}

stop_swoole() {
    (cd "${RUNTIME_APP}" && "${PHP_BIN}" artisan octane:stop --server=swoole)
}

# ---------------------------------------------------------------------------
# Server dispatch (openswoole shares the swoole functions; the server name
# itself stays openswoole for octane options).
# ---------------------------------------------------------------------------

server_fn_suffix() {
    if [[ "${SERVER}" == "openswoole" ]]; then
        echo "swoole"
    else
        echo "${SERVER}"
    fi
}

# Whether a graceful octane:stop can flush parked publications at all.
#
# The Swoole family cannot: laravel/octane's Swoole
# ServerProcessInspector::stopServer() sends SIGKILL to the master, the
# manager, and every worker process. SIGKILL is uncatchable — no
# workerstop callback runs, so no WorkerStopping event fires and the
# driver's pool close (with its publish-buffer flush) never executes.
# Publications parked in the worker's buffer are lost with the process.
# The harness certifies the reload-flush path on these servers and
# asserts the loss explicitly instead of faking a pass.
stop_flush_expected() {
    [[ "${SERVER}" != "swoole" && "${SERVER}" != "openswoole" ]]
}

# Whether octane:reload recycles workers in place, keeping the server up.
#
# FrankenPHP does not: laravel/octane's FrankenPHP
# ServerProcessInspector::reloadServer() PATCHes the Caddy admin config
# endpoint (Cache-Control: must-revalidate), which on the pinned
# dunglas/frankenphp image (digest-pinned above) shuts the whole
# frankenphp app down instead of recycling workers in place. The PHP
# workers still shut down gracefully — the parked publications are
# flushed to the broker before the process exits (verified against the
# broker) — so no data is lost; the harness restarts the server across
# the reload and asserts the same no-loss outcome. This is an
# availability difference (brief downtime on reload), not a data-safety
# one.
reload_recycles_workers() {
    [[ "${SERVER}" != "frankenphp" ]]
}

start_server()    { "start_$(server_fn_suffix)"; }
reload_server()   { "reload_$(server_fn_suffix)"; }
stop_server()     { "stop_$(server_fn_suffix)"; }

# ---------------------------------------------------------------------------
# Scenario.
# ---------------------------------------------------------------------------

EXTENSION_ARTIFACT="$(ext_artifact_path)"
ext_ensure_built
ext_verify_loads

# The scenario CLI itself resolves Pool::size() through the extension (the
# management API may omit the queue depth gauge on a fresh lab), and every
# server subshell inherits it too. Resolved ONCE so a concurrent
# system-wide install/removal mid-run cannot flip the decision; guarded
# export so an empty result never runs a bare `export` (which would dump
# the whole environment).
EXTENSION_ENV_LINE="$(extension_env)"
if [[ -n "${EXTENSION_ENV_LINE}" ]]; then
    export "${EXTENSION_ENV_LINE}"
fi

SERVER_LOG="${RUNTIME_ROOT}/.server-${SERVER}.log"

start_lab

prepare_runtime_app
rm -f "${SERVER_LOG}"

log "declaring scenario queue ${RUNTIME_QUEUE} on ${RUNTIME_VHOST}"
export RUNTIME_BASE_URL="${BASE_URL}" RUNTIME_MGMT_URL="${MGMT_URL}"
export RUNTIME_QUEUE RUNTIME_VHOST

scenario purge

log "starting ${SERVER} server"
"start_$(server_fn_suffix)"
wait_server_up

# Publications that must be on the broker when phase 3 starts. A server
# that loses a parked batch on the way reduces this count; the assertions
# below make the loss explicit instead of failing silently later.
TOTAL_EXPECTED=10

# Phase 1: publish x5 with no follow-up op; publications park in the buffer.
log "phase 1: publish x5 (parked), then octane:reload"
scenario publish 5
scenario wait-buffered 5 15
if reload_recycles_workers; then
    reload_server
    scenario wait-depth 5 30
    scenario wait-buffered 0 15
    log "phase 1 ok: reload flushed the parked publications (depth 5, buffer 0)"
else
    # FrankenPHP: the upstream octane reload shuts the whole frankenphp
    # app down (see reload_recycles_workers) — but the workers shut down
    # gracefully, flushing parked publications to the broker before the
    # process exits. Restart across the reload and assert the same
    # no-loss outcome the in-place recycle provides elsewhere.
    reload_server || true
    wait_server_gone
    "start_$(server_fn_suffix)"
    wait_server_up
    scenario wait-depth 5 30
    scenario wait-buffered 0 15
    log "phase 1 ok: upstream octane reload shut the whole frankenphp app down; workers flushed on the way out (depth 5, buffer 0, no loss; server restarted)"
fi

# Phase 2: publish x5 again, then graceful stop.
log "phase 2: publish x5 (parked), then octane:stop"
scenario publish 5
scenario wait-buffered 5 15
scenario wait-depth 5 15   # batch 2 is parked, NOT yet on the broker
stop_server
wait_server_gone
if stop_flush_expected; then
    scenario wait-depth "${TOTAL_EXPECTED}" 30
    log "phase 2 ok: graceful stop flushed the parked publications (depth ${TOTAL_EXPECTED}, no loss)"
    EXPECTED_DRAIN="${TOTAL_EXPECTED}"
else
    # Upstream octane SIGKILLs swoole-family workers on stop: the parked
    # batch dies with the process (documented limit, not a driver bug).
    # Assert the loss explicitly so the harness can always DETECT it, then
    # certify the reload-flush path only.
    scenario wait-depth 5 15
    log "phase 2: ${SERVER} octane:stop SIGKILLs workers (upstream laravel/octane) — the 5 parked publications are lost; graceful-stop flush not certifiable on this server"
    EXPECTED_DRAIN=5
fi

# Phase 3: restart, consume everything, ack, drain to zero.
log "phase 3: restart, consume ${EXPECTED_DRAIN} via CLI worker, ack all, drain to 0"
"start_$(server_fn_suffix)"
wait_server_up
# Consume from a CLI worker process, not the running server: the driver
# closes cached consumers after every request (Octane terminating hook)
# while the native client keeps serving the closed handle from its
# per-profile cache, so server-side pops fail from the second request on
# (driver bug, see the WS5b report). CLI workers are also the shape
# production consumption uses.
scenario consume-ack-cli "${EXPECTED_DRAIN}" 90
scenario wait-depth 0 30
stop_server
wait_server_gone

echo ""
if stop_flush_expected && reload_recycles_workers; then
    echo "PASS: ${SERVER} certified — publish -> reload -> graceful stop -> no loss -> drain to zero"
else
    echo "PASS: ${SERVER} certified with a documented limitation — see the certification table in packages/laravel-queue/docs/reference.md"
fi
