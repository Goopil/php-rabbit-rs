#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PACKAGE_DIR="${PROJECT_ROOT}/packages/laravel-queue"

cd "${PACKAGE_DIR}"

if ! command -v php &> /dev/null; then
    echo "ERROR: php not found"
    exit 1
fi

php -d memory_limit=512M vendor/bin/pest tests/Feature --filter="Octane" --testdox

echo "Octane lifecycle tests passed"
