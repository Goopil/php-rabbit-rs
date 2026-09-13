#!/usr/bin/env bash
# Wait for the RabbitMQ lab to become ready (120s budget), then exit 0.
# Silences lab-ready.sh output; errors go to stderr with exit 1.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for _ in $(seq 1 120); do
    if "${SCRIPT_DIR}/lab-ready.sh" >/dev/null 2>&1; then
        echo "Lab is ready."
        exit 0
    fi
    sleep 1
done

echo "Lab did not become ready after 120s." >&2
exit 1
