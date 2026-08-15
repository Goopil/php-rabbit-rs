#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LAB_DIR="${PROJECT_ROOT}/lab/rabbitmq"

command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 1; }

if docker compose version >/dev/null 2>&1; then
    DC="docker compose"
elif command -v docker-compose >/dev/null 2>&1; then
    DC="docker-compose"
else
    echo "ERROR: docker compose is required" >&2
    exit 1
fi

cd "${LAB_DIR}"

echo "Stopping RabbitMQ lab..."
${DC} --profile with-plugin --profile without-plugin down --remove-orphans -v
echo "Lab stopped."
