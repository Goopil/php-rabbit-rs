#!/usr/bin/env bash
set -euo pipefail

SERVER="${1:-frankenphp}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PACKAGE_DIR="${PROJECT_ROOT}/packages/laravel-queue"

echo "Testing Octane lifecycle with ${SERVER}..."

cd "${PACKAGE_DIR}"

if ! command -v php &> /dev/null; then
    echo "ERROR: php not found"
    exit 1
fi

# Load the extension from the build output if it exists.
EXTENSION_FLAGS=""
case "$(uname -s)" in
    Darwin) SO_PATH="${PROJECT_ROOT}/target/release/librabbit_rs_php.dylib" ;;
    Linux)  SO_PATH="${PROJECT_ROOT}/target/release/librabbit_rs_php.so" ;;
    *)      echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
if [[ -f "${SO_PATH}" ]]; then
    EXTENSION_FLAGS="-d extension=${SO_PATH}"
fi

php ${EXTENSION_FLAGS} -d memory_limit=512M vendor/bin/phpunit --filter OctaneLifecycleTest

echo "Octane lifecycle tests passed for ${SERVER}"
