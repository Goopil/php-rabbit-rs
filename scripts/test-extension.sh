#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"

ROOT_DIR="$(ext_project_root)"
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

major_minor_version() {
    local tool_name="$1"
    local version="$2"

    if [[ ! "${version}" =~ ^([0-9]+)\.([0-9]+)(\.|$) ]]; then
        echo "${tool_name} returned invalid version '${version}'; expected numeric major.minor" >&2
        return 1
    fi

    printf '%s.%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
}

PHP_BIN_PATH="$(resolve_tool php "${PHP_BIN:-php}")"
PHP_CONFIG_PATH="$(resolve_tool php-config "${PHP_CONFIG:-php-config}")"

PHP_VERSION="$("${PHP_BIN_PATH}" -r 'echo PHP_VERSION;')"
PHP_CONFIG_VERSION="$("${PHP_CONFIG_PATH}" --version)"
PHP_MAJOR_MINOR="$(major_minor_version php "${PHP_VERSION}")"
PHP_CONFIG_MAJOR_MINOR="$(major_minor_version php-config "${PHP_CONFIG_VERSION}")"

PHP_MAJOR="${PHP_MAJOR_MINOR%%.*}"
PHP_MINOR="${PHP_MAJOR_MINOR#*.}"
if (( 10#${PHP_MAJOR} < 8 || (10#${PHP_MAJOR} == 8 && 10#${PHP_MINOR} < 4) )); then
    echo "PHP 8.4 or newer is required; php reports ${PHP_VERSION}" >&2
    exit 1
fi

if [[ "${PHP_MAJOR_MINOR}" != "${PHP_CONFIG_MAJOR_MINOR}" ]]; then
    echo "php (${PHP_VERSION}) and php-config (${PHP_CONFIG_VERSION}) must use the same major.minor version" >&2
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

# --- Extension artifact (via shared helper) ---
ext_ensure_built
ARTIFACT="$(ext_artifact_path)"

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

PHP_EXT_DIR="${ROOT_DIR}/crates/rabbit-rs-php"

# --- Pest tests ---
if [[ ! -d "${PHP_EXT_DIR}/vendor" ]]; then
    (cd "${PHP_EXT_DIR}" && composer update --no-interaction --no-security-blocking)
fi

echo "Running Pest tests..."
(cd "${PHP_EXT_DIR}" && "${PHP_BIN_PATH}" -d "extension=${ARTIFACT}" vendor/bin/pest)

# --- PHPT tests (all *.phpt scenarios) ---
shopt -s nullglob
PHPT_TESTS=("${PHPT_DIR}"/*.phpt)
shopt -u nullglob
if (( ${#PHPT_TESTS[@]} == 0 )); then
    echo "No PHPT tests found in ${PHPT_DIR}" >&2
    exit 1
fi

export NO_INTERACTION=1
export REPORT_EXIT_STATUS=1
export TEST_PHP_EXECUTABLE="${PHP_BIN_PATH}"

echo "Running PHPT tests (${#PHPT_TESTS[@]} scenarios)..."
for PHPT_TEST in "${PHPT_TESTS[@]}"; do
    "${PHP_BIN_PATH}" "${RUN_TESTS}" -n -d "extension=${ARTIFACT}" "${PHPT_TEST}"
done
