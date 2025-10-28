#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
source "$ROOT/scripts/common.sh"

NAME=${BENCH_RABBIT_NAME:-rabbitmq-benchmark}
IMAGE=${RABBIT_IMAGE:-rabbitmq:3.13-management}
AMQP_PORT=${BENCH_AMQP_PORT:-5672}
MGMT_PORT=${BENCH_MGMT_PORT:-15672}
TIMEOUT=${BENCH_RABBIT_TIMEOUT:-60}
VOLUME_NAME=${BENCH_RABBIT_VOLUME:-rabbitmq_benchmark_data}
RABBIT_USER=${BENCH_RABBIT_USER:-guest}
RABBIT_PASSWORD=${BENCH_RABBIT_PASSWORD:-guest}

TMPDIR=$(mktemp -d)
COMPOSE_FILE="$TMPDIR/docker-compose.yml"
common::write_rabbit_compose "$COMPOSE_FILE" "$NAME" "$IMAGE" "$AMQP_PORT" "$MGMT_PORT" "$VOLUME_NAME" "$RABBIT_USER" "$RABBIT_PASSWORD"
COMPOSE_BIN=$(common::compose_bin)

cleanup() {
  common::compose_down "$COMPOSE_BIN" "$COMPOSE_FILE" -v --remove-orphans
  rm -rf "$TMPDIR" || true
}
trap cleanup EXIT

(docker rm -f "$NAME" >/dev/null 2>&1) || true

common::compose_up "$COMPOSE_BIN" "$COMPOSE_FILE"

common::wait_for_health "$NAME" "$TIMEOUT"
common::await_startup "$NAME" 60

EXT_PATH="$(common::resolve_extension "$ROOT")"
export LIB_PATH="$EXT_PATH"

(
  cd "$ROOT/php/benchmarks"
  if [[ "${BENCH_FORCE_COMPOSER:-0}" == "1" || ! -d "vendor" ]]; then
    if ! command -v composer >/dev/null 2>&1; then
      echo "Composer is required to install benchmark dependencies." >&2
      exit 1
    fi
    composer install --no-interaction --no-progress
  fi

  php -d extension="$EXT_PATH" src/run-benchmarks.php "$@"
)
