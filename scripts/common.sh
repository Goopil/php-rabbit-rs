#!/usr/bin/env bash
# Shared helpers for RabbitRs dev scripts.

# Determine docker compose command (docker-compose legacy or docker compose plugin).
common::compose_bin() {
  if command -v docker-compose >/dev/null 2>&1; then
    echo "docker-compose"
  else
    echo "docker compose"
  fi
}

# Render a minimal docker-compose file for RabbitMQ.
# Args: <file> <service-name> <image> <amqp-port> <mgmt-port> <volume-name> <user> <password>
common::write_rabbit_compose() {
  local file="$1"
  local service="$2"
  local image="$3"
  local amqp_port="$4"
  local mgmt_port="$5"
  local volume_name="$6"
  local user="$7"
  local password="$8"

  cat >"$file" <<YAML
services:
  rabbit:
    image: ${image}
    container_name: ${service}
    hostname: ${service}
    ports:
      - "${amqp_port}:5672"
      - "${mgmt_port}:15672"
    environment:
      RABBITMQ_ERLANG_COOKIE: rabbitrs-cookie-please-change
      RABBITMQ_DEFAULT_USER: ${user}
      RABBITMQ_DEFAULT_PASS: ${password}
      RABBITMQ_DEFAULT_VHOST: /
      RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS: "-rabbit loopback_users []"
    volumes:
      - ${volume_name}:/var/lib/rabbitmq
    healthcheck:
      test: ["CMD", "rabbitmq-diagnostics", "-q", "ping"]
      interval: 2s
      timeout: 2s
      retries: 30
      start_period: 5s
volumes:
  ${volume_name}: {}
YAML
}

# Bring up the compose stack quietly.
common::compose_up() {
  local bin="$1"
  local file="$2"
  shift 2
  $bin -f "$file" up -d "$@" >/dev/null
}

# Bring down the compose stack quietly.
common::compose_down() {
  local bin="$1"
  local file="$2"
  shift 2
  $bin -f "$file" down "$@" >/dev/null 2>&1 || true
}

# Wait for the RabbitMQ container to become healthy.
# Args: <container-name> <timeout-seconds>
common::wait_for_health() {
  local name="$1"
  local timeout="$2"

  printf 'Waiting for RabbitMQ (%s) to become healthy' "$name"
  for i in $(seq 1 "$timeout"); do
    local status
    status=$(docker inspect -f '{{ .State.Health.Status }}' "$name" 2>/dev/null || echo "starting")
    if [[ "$status" == "healthy" ]]; then
      echo " — ready."
      return 0
    fi
    if ! docker ps --format '{{.Names}}' | grep -q "^${name}$"; then
      echo " — container exited unexpectedly." >&2
      docker logs --tail 200 "$name" >&2 || true
      return 1
    fi
    printf '.'
    sleep 1
  done

  echo " — timeout waiting for health." >&2
  docker logs --tail 200 "$name" >&2 || true
  return 1
}

# Wait for rabbitmqctl await_startup to succeed.
# Args: <container-name> <timeout-seconds>
common::await_startup() {
  local name="$1"
  local timeout="$2"

  printf 'Waiting for RabbitMQ app to start'
  for i in $(seq 1 "$timeout"); do
    if docker exec "$name" rabbitmqctl await_startup >/dev/null 2>&1; then
      echo " — started."
      return 0
    fi
    printf '.'
    sleep 1
  done

  echo " — timeout waiting for app start." >&2
  docker logs --tail 200 "$name" >&2 || true
  return 1
}

# Discover compiled extension path, preferring debug builds.
# Honors LIB_PATH if already set in the environment.
common::resolve_extension() {
  local root="$1"

  if [[ -n "${LIB_PATH:-}" ]]; then
    echo "$LIB_PATH"
    return 0
  fi

  local ext
  case "$(uname -s)" in
    Darwin*) ext="dylib" ;;
    MINGW*|MSYS*|CYGWIN*) ext="dll" ;;
    *) ext="so" ;;
  esac

  local debug_candidate="$root/target/debug/librabbit_rs.${ext}"
  local release_candidate="$root/target/release/librabbit_rs.${ext}"

  if [[ -f "$debug_candidate" ]]; then
    echo "$debug_candidate"
    return 0
  fi
  if [[ -f "$release_candidate" ]]; then
    echo "$release_candidate"
    return 0
  fi

  echo "Unable to locate RabbitRs extension (looked for ${debug_candidate} and ${release_candidate}). Run 'cargo build' first." >&2
  return 1
}
