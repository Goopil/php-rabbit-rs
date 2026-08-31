#!/usr/bin/env bash
# Shared RabbitMQ lab helpers: project paths and Docker Compose detection.

_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${_LIB_DIR}/.." && pwd)"
LAB_DIR="${PROJECT_ROOT}/lab/rabbitmq"

# Detect docker compose and set DC to either "docker compose" or "docker-compose".
lab_dc() {
    command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 1; }

    if docker compose version >/dev/null 2>&1; then
        DC="docker compose"
    elif command -v docker-compose >/dev/null 2>&1; then
        DC="docker-compose"
    else
        echo "ERROR: docker compose is required" >&2
        exit 1
    fi
}
