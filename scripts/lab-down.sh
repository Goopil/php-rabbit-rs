#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=lib-lab.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib-lab.sh"

lab_dc

cd "${LAB_DIR}"

echo "Stopping RabbitMQ lab..."
${DC} --profile with-plugin --profile without-plugin down --remove-orphans -v
echo "Lab stopped."
