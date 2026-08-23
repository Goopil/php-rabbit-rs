#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

PROJECT_ROOT="$(ext_project_root)"
PACKAGE_DIR="${PROJECT_ROOT}/packages/laravel-queue"

cd "${PACKAGE_DIR}"

if ! command -v php &> /dev/null; then
    echo "ERROR: php not found"
    exit 1
fi

# Octane lifecycle tests use fake classes (no extension needed).
# Use -n to ignore system ini files, preventing double-loading warnings.
php -n -d memory_limit=512M vendor/bin/pest tests/Feature --filter="Octane" --testdox

echo "Octane lifecycle tests passed"
