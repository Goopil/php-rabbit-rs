#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! curl -s -o /dev/null -u rabbit_rs:rabbit_rs_lab http://localhost:15672/api/overview; then
    echo "RabbitMQ not reachable at localhost:5672."
    echo "Start the lab with: ./scripts/lab-up.sh"
    exit 1
fi

if [[ ! -d vendor ]]; then
    composer install --no-interaction
fi

php -d xdebug.mode=off src/run-benchmarks.php "$@"
