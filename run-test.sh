#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
source "$ROOT/scripts/common.sh"

NAME=${RABBIT_NAME:-rabbitmq-test}
IMAGE=${RABBIT_IMAGE:-rabbitmq:3.13-management}
AMQP_PORT=${AMQP_PORT:-5672}
MGMT_PORT=${MGMT_PORT:-15672}
TIMEOUT=${RABBIT_TIMEOUT:-60}
VOLUME_NAME=${RABBIT_VOLUME:-rabbitmq_test_data}
RABBIT_USER=${RABBIT_USER:-test}
RABBIT_PASSWORD=${RABBIT_PASSWORD:-test}

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

export E2E_CAN_RESTART_BROKER=1
export E2E_RABBIT_SERVICE=$NAME
LIB_PATH="$(common::resolve_extension "$ROOT")"
export LIB_PATH

(
  cd "$ROOT/php/tests"
  php -d extension="$LIB_PATH" ./vendor/bin/pest "$@"
)
