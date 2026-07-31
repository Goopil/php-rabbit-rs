#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPT_DIR="${ROOT_DIR}/crates/rabbit-rs-php/tests/phpt"

resolve_tool() {
    local tool_name="$1"
    local candidate="$2"

    if ! command -v "${candidate}" >/dev/null 2>&1; then
        echo "${tool_name} executable not found: ${candidate}" >&2
        return 1
    fi

    command -v "${candidate}"
}

PHP_BIN_PATH="$(resolve_tool php "${PHP_BIN:-php}")"
PHP_CONFIG_PATH="$(resolve_tool php-config "${PHP_CONFIG:-php-config}")"

PHP_VERSION="$("${PHP_BIN_PATH}" -r 'echo PHP_VERSION;')"
PHP_CONFIG_VERSION="$("${PHP_CONFIG_PATH}" --version)"
if [[ "${PHP_VERSION}" != "${PHP_CONFIG_VERSION}" ]]; then
    echo "php (${PHP_VERSION}) and php-config (${PHP_CONFIG_VERSION}) versions do not match" >&2
    exit 1
fi

PHP_BUILD_DIR="$("${PHP_CONFIG_PATH}" --prefix)/lib/php/build"
if [[ ! -d "${PHP_BUILD_DIR}" ]]; then
    echo "PHP build directory not found: ${PHP_BUILD_DIR}" >&2
    exit 1
fi

RUN_TESTS_SOURCE="$(find "${PHP_BUILD_DIR}" -type f -name run-tests.php -print -quit)"
if [[ -z "${RUN_TESTS_SOURCE}" ]]; then
    echo "run-tests.php not found below ${PHP_BUILD_DIR}" >&2
    exit 1
fi

RUN_TESTS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/rabbit-rs-phpt.XXXXXX")"
trap 'rm -rf "${RUN_TESTS_DIR}"' EXIT
RUN_TESTS="${RUN_TESTS_DIR}/run-tests.php"
cp "${RUN_TESTS_SOURCE}" "${RUN_TESTS}"

case "$(uname -s)" in
    Darwin)
        ARTIFACT="${ROOT_DIR}/target/release/librabbit_rs_php.dylib"
        ;;
    Linux)
        ARTIFACT="${ROOT_DIR}/target/release/librabbit_rs_php.so"
        ;;
    *)
        echo "unsupported operating system: $(uname -s)" >&2
        exit 1
        ;;
esac

if [[ ! -f "${ARTIFACT}" ]]; then
    echo "extension artifact not found: ${ARTIFACT}" >&2
    exit 1
fi

RABBIT_RS_EXPECTED_VERSION="$({
    cargo metadata --manifest-path "${ROOT_DIR}/Cargo.toml" --no-deps --format-version=1
} | "${PHP_BIN_PATH}" -r '
    $metadata = json_decode(stream_get_contents(STDIN), true, flags: JSON_THROW_ON_ERROR);
    foreach ($metadata["packages"] as $package) {
        if ($package["name"] === "rabbit-rs-php") {
            echo $package["version"];
            exit(0);
        }
    }
    fwrite(STDERR, "rabbit-rs-php package missing from Cargo metadata\n");
    exit(1);
')"
export RABBIT_RS_EXPECTED_VERSION

shopt -s nullglob
if [[ $# -gt 0 ]]; then
    TESTS=("${PHPT_DIR}"/*"$1"*.phpt)
else
    TESTS=("${PHPT_DIR}"/*.phpt)
fi

if [[ ${#TESTS[@]} -eq 0 ]]; then
    echo "no PHPT tests matched" >&2
    exit 1
fi

export NO_INTERACTION=1
export REPORT_EXIT_STATUS=1
export TEST_PHP_EXECUTABLE="${PHP_BIN_PATH}"

"${PHP_BIN_PATH}" "${RUN_TESTS}" -n -d "extension=${ARTIFACT}" "${TESTS[@]}"
