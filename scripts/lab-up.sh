#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib-lab.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib-lab.sh"

lab_dc

PROFILE="${1:-with-plugin}"

if [[ "${PROFILE}" != "with-plugin" && "${PROFILE}" != "without-plugin" ]]; then
    echo "Usage: $0 [with-plugin|without-plugin]" >&2
    exit 1
fi

cd "${LAB_DIR}"

echo "Starting RabbitMQ lab (profile: ${PROFILE})..."
${DC} --profile "${PROFILE}" down --remove-orphans -v 2>/dev/null || true
${DC} --profile "${PROFILE}" up -d --build

if [[ "${PROFILE}" == "without-plugin" ]]; then
    echo "Joining nodes to cluster (manual clustering for RabbitMQ 4.3)..."
    for i in $(seq 1 60); do
        if docker exec rabbitrs-rabbitmq-1-np-1 rabbitmq-diagnostics -q ping >/dev/null 2>&1; then
            break
        fi
        sleep 2
    done
    docker exec rabbitrs-rabbitmq-2-np-1 rabbitmqctl stop_app 2>/dev/null
    docker exec rabbitrs-rabbitmq-2-np-1 rabbitmqctl join_cluster rabbit@rabbitmq-1 2>/dev/null
    docker exec rabbitrs-rabbitmq-2-np-1 rabbitmqctl start_app 2>/dev/null
    docker exec rabbitrs-rabbitmq-3-np-1 rabbitmqctl stop_app 2>/dev/null
    docker exec rabbitrs-rabbitmq-3-np-1 rabbitmqctl join_cluster rabbit@rabbitmq-1 2>/dev/null
    docker exec rabbitrs-rabbitmq-3-np-1 rabbitmqctl start_app 2>/dev/null
    echo "Cluster formed."
fi

echo ""
echo "Lab starting. Use ./scripts/lab-ready.sh to verify readiness."
echo "  AMQP:                localhost:5672, localhost:5673, localhost:5675"
echo "  Management UI:        http://localhost:15672  (admin / admin_lab)"
echo "  Prometheus:          http://localhost:9091"
