#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! docker compose ps --status running | grep -q rabbitmq-benchmark; then
    echo "Starting RabbitMQ..."
    docker compose up -d --wait
fi

if [ ! -d vendor ]; then
    composer install --no-interaction
fi

php src/run-benchmarks.php "$@"
