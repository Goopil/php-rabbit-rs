#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib-lab.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib-lab.sh"

lab_dc

PROFILE="${1:-with-plugin}"

if [[ "${PROFILE}" != "with-plugin" && "${PROFILE}" != "without-plugin" && "${PROFILE}" != "with-tls" ]]; then
    echo "Usage: $0 [with-plugin|without-plugin|with-tls]" >&2
    exit 1
fi

cd "${LAB_DIR}"

# The TLS node needs certificates that are deliberately not committed: they
# are regenerated on every lab-up so key material never outlives the lab.
COMPOSE_PROFILES=(--profile "${PROFILE}")
if [[ "${PROFILE}" == "with-tls" ]]; then
    echo "Generating TLS lab certificates..."
    "${LAB_DIR}/tls/generate.sh"
    # TLS tests run against the full lab, so the plugin cluster comes up too.
    COMPOSE_PROFILES=(--profile with-plugin --profile with-tls)
fi

echo "Starting RabbitMQ lab (profile: ${PROFILE})..."
${DC} "${COMPOSE_PROFILES[@]}" down --remove-orphans -v 2>/dev/null || true
${DC} "${COMPOSE_PROFILES[@]}" up -d --build

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
if [[ "${PROFILE}" == "with-tls" ]]; then
    echo "  AMQPS:               localhost:5671 (lab CA: lab/rabbitmq/tls/generated/lab-ca.pem)"
fi
echo "  Management UI:        http://localhost:15672  (admin / admin_lab)"
echo "  Prometheus:          http://localhost:9091"
echo "  Toxiproxy API:       http://localhost:18474"
