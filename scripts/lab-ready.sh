#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib-lab.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib-lab.sh"

MGMT_PORT="${RABBIT_RS_LAB_MGMT:-15672}"
PROM_PORT="${RABBIT_RS_LAB_PROM:-9091}"
TOXIPROXY_PORT="${RABBIT_RS_LAB_TOXI:-18474}"

ADMIN_USER="${RABBIT_RS_LAB_ADMIN_USER:-admin}"
ADMIN_PASS="${RABBIT_RS_LAB_ADMIN_PASS:-admin_lab}"
RABBITMQ_USER="${RABBIT_RS_LAB_USER:-rabbit_rs}"
RABBITMQ_PASS="${RABBIT_RS_LAB_PASS:-rabbit_rs_lab}"
VHOST_ORDERS="/orders-eu"
VHOST_BILLING="/billing"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "  ok: $*"
}

echo "Checking RabbitMQ lab readiness..."

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v jq   >/dev/null 2>&1 || fail "jq is required"

echo "Checking Docker Compose services are running..."
cd "${LAB_DIR}"
lab_dc

SERVICES=$(${DC} --profile with-plugin --profile without-plugin ps --format json 2>/dev/null || true)
RUNNING_SERVICES=$(echo "${SERVICES}" | jq -r 'if type == "array" then [.[] | .Service] else [.Service] end | .[]' 2>/dev/null || true)

for svc in rabbitmq-1 rabbitmq-2 rabbitmq-3 toxiproxy prometheus; do
    echo "${RUNNING_SERVICES}" | grep -q "^${svc}$" || echo "${RUNNING_SERVICES}" | grep -q "^${svc}-np$" || fail "service ${svc} is not running"
done
ok "all compose services present"

echo "Checking 3 RabbitMQ nodes are running and clustered..."
MGMT_OK=false
for i in $(seq 1 60); do
    RESP=$(curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" "http://localhost:${MGMT_PORT}/api/overview" 2>/dev/null || true)
    if [[ -n "${RESP}" ]]; then
        MGMT_OK=true
        break
    fi
    sleep 2
done
[[ "${MGMT_OK}" == true ]] || fail "management API on port ${MGMT_PORT} is not responding"
ok "management API responding"

RABBIT_VERSION=$(echo "${RESP}" | jq -r '.rabbitmq_version // empty')
[[ -n "${RABBIT_VERSION}" ]] || fail "cannot read RabbitMQ version"
ok "RabbitMQ version: ${RABBIT_VERSION}"

echo "Checking cluster has 3 nodes..."
CLUSTER_NODES=$(curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" "http://localhost:${MGMT_PORT}/api/nodes" 2>/dev/null | jq 'length')
[[ "${CLUSTER_NODES}" -eq 3 ]] || fail "expected 3 cluster nodes, got ${CLUSTER_NODES}"
ok "cluster has 3 nodes"

echo "Checking quorum queues can be created (quorum leader)..."
QUEUE_HTTP=$(curl -s -o /dev/null -w '%{http_code}' -u "${ADMIN_USER}:${ADMIN_PASS}" \
    -X PUT -H "content-type: application/json" \
    -d '{"durable":true,"arguments":{"x-queue-type":"quorum"}}' \
    "http://localhost:${MGMT_PORT}/api/queues/${VHOST_ORDERS//\//%2f}/readiness-check" 2>/dev/null || true)
[[ "${QUEUE_HTTP}" == "201" || "${QUEUE_HTTP}" == "204" ]] || fail "cannot create quorum queue on ${VHOST_ORDERS} (HTTP ${QUEUE_HTTP})"
ok "quorum queue creation works"

echo "Checking vhosts..."
VHOSTS=$(curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" "http://localhost:${MGMT_PORT}/api/vhosts" 2>/dev/null)
VHOST_NAMES=$(echo "${VHOSTS}" | jq -r '.[].name')
echo "${VHOST_NAMES}" | grep -q "${VHOST_ORDERS}"  || fail "vhost ${VHOST_ORDERS} not found"
echo "${VHOST_NAMES}" | grep -q "${VHOST_BILLING}" || fail "vhost ${VHOST_BILLING} not found"
ok "both vhosts ${VHOST_ORDERS} and ${VHOST_BILLING} present"

echo "Checking user has limited permissions..."
USER_PERMS_ORDERS=$(curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" "http://localhost:${MGMT_PORT}/api/permissions/${VHOST_ORDERS//\//%2f}/${RABBITMQ_USER}" 2>/dev/null | jq -r '.configure')
USER_PERMS_BILLING=$(curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" "http://localhost:${MGMT_PORT}/api/permissions/${VHOST_BILLING//\//%2f}/${RABBITMQ_USER}" 2>/dev/null | jq -r '.configure')
[[ "${USER_PERMS_ORDERS}" != ".*" ]] || fail "user should not have wildcard configure permission on ${VHOST_ORDERS}"
[[ "${USER_PERMS_BILLING}" != ".*" ]] || fail "user should not have wildcard configure permission on ${VHOST_BILLING}"
ok "user has limited permissions"

echo "Checking Prometheus is scraping..."
PROM_OK=false
for i in $(seq 1 30); do
    PROM_TARGETS=$(curl -sf "http://localhost:${PROM_PORT}/api/v1/targets?state=active" 2>/dev/null || true)
    if [[ -n "${PROM_TARGETS}" ]] && echo "${PROM_TARGETS}" | jq -e '.data.activeTargets | length > 0' >/dev/null 2>&1; then
        PROM_OK=true
        break
    fi
    sleep 1
done
[[ "${PROM_OK}" == true ]] || fail "Prometheus has no active targets"
ok "Prometheus is scraping"

echo "Checking Toxiproxy..."
TOXI_OK=false
for i in $(seq 1 30); do
    TOXI_RESP=$(curl -sf "http://localhost:${TOXIPROXY_PORT}/proxies/rabbitmq-1" 2>/dev/null || true)
    if [[ -n "${TOXI_RESP}" ]]; then
        TOXI_OK=true
        break
    fi
    sleep 1
done
[[ "${TOXI_OK}" == true ]] || fail "Toxiproxy API on port ${TOXIPROXY_PORT} is not responding"
TOXI_UPSTREAM=$(echo "${TOXI_RESP}" | jq -r '.upstream // empty')
[[ "${TOXI_UPSTREAM}" == "rabbitmq-1:5672" ]] \
    || fail "port ${TOXIPROXY_PORT} is answered by a foreign Toxiproxy (rabbitmq-1 upstream: ${TOXI_UPSTREAM:-none})"
ok "Toxiproxy API responding (lab fingerprint rabbitmq-1 -> rabbitmq-1:5672)"

echo "Checking delayed message exchange plugin..."
CONTAINER_NAME="rabbitrs-rabbitmq-1-1"
if ! docker exec "${CONTAINER_NAME}" rabbitmq-diagnostics -q ping >/dev/null 2>&1; then
    CONTAINER_NAME="rabbitrs-rabbitmq-1-np-1"
fi
if docker exec "${CONTAINER_NAME}" rabbitmq-diagnostics -q ping >/dev/null 2>&1; then
    PLUGIN_ENABLED=$(docker exec "${CONTAINER_NAME}" rabbitmq-plugins list --enabled 2>/dev/null || true)
    if echo "${PLUGIN_ENABLED}" | grep -q "rabbitmq_delayed_message_exchange"; then
        ok "delayed message exchange plugin is enabled"
    else
        echo "  warn: delayed message exchange plugin not enabled (profile: without-plugin)"
    fi
fi

echo "Cleaning up readiness check queue..."
curl -sf -u "${ADMIN_USER}:${ADMIN_PASS}" \
    -X DELETE \
    "http://localhost:${MGMT_PORT}/api/queues/${VHOST_ORDERS//\//%2f}/readiness-check" >/dev/null 2>&1 || true

echo ""
echo "LAB READY: 3-node RabbitMQ ${RABBIT_VERSION} cluster, 2 vhosts, Prometheus and Toxiproxy operational."
