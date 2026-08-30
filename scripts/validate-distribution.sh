#!/usr/bin/env bash
set -euo pipefail

# validate-distribution.sh — verify package metadata, PIE matrix, and version coherence.
#
# Exits non-zero if any check fails. Run before tagging a release or publishing
# the extension / Laravel package to Packagist.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_COMPOSER="${ROOT_DIR}/composer.json"
LARAVEL_COMPOSER="${ROOT_DIR}/packages/laravel-queue/composer.json"
CARGO_TOML="${ROOT_DIR}/Cargo.toml"
PIE_MATRIX="${ROOT_DIR}/release/pie-matrix.json"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

need_file() {
    local path="$1"
    if [[ ! -f "${path}" ]]; then
        fail "required file missing: ${path}"
    fi
}

need_cmd() {
    local cmd="$1"
    if ! command -v "${cmd}" >/dev/null 2>&1; then
        fail "required command not found: ${cmd}"
    fi
}

echo "==> Checking required files and tools"
need_file "${ROOT_COMPOSER}"
need_file "${LARAVEL_COMPOSER}"
need_file "${CARGO_TOML}"
need_file "${PIE_MATRIX}"
need_cmd jq
need_cmd grep

echo "==> Checking root composer.json metadata"
root_name="$(jq -r '.name' "${ROOT_COMPOSER}")"
[[ "${root_name}" == "goopil/rabbit-rs-native" ]] \
    || fail "root package name is '${root_name}', expected 'goopil/rabbit-rs-native'"
ok "root package name: ${root_name}"

root_type="$(jq -r '.type' "${ROOT_COMPOSER}")"
[[ "${root_type}" == "php-ext" ]] \
    || fail "root package type is '${root_type}', expected 'php-ext'"
ok "root package type: ${root_type}"

ext_name="$(jq -r '.["php-ext"]."extension-name"' "${ROOT_COMPOSER}")"
[[ "${ext_name}" == "rabbit_rs" ]] \
    || fail "extension-name is '${ext_name}', expected 'rabbit_rs'"
ok "extension-name: ${ext_name}"

echo "==> Checking download-url-method"
download_methods="$(jq -r '.["php-ext"]."download-url-method" | length' "${ROOT_COMPOSER}")"
[[ "${download_methods}" -eq 1 ]] \
    || fail "download-url-method must contain exactly 1 entry, found ${download_methods}"
download_method="$(jq -r '.["php-ext"]."download-url-method"[0]' "${ROOT_COMPOSER}")"
[[ "${download_method}" == "pre-packaged-binary" ]] \
    || fail "download-url-method[0] is '${download_method}', expected 'pre-packaged-binary'"
ok "download-url-method: [${download_method}]"

echo "==> Checking thread-safety support"
support_nts="$(jq -r '.["php-ext"]."support-nts"' "${ROOT_COMPOSER}")"
support_zts="$(jq -r '.["php-ext"]."support-zts"' "${ROOT_COMPOSER}")"
[[ "${support_nts}" == "true" ]] \
    || fail "support-nts is '${support_nts}', expected 'true'"
[[ "${support_zts}" == "false" ]] \
    || fail "support-zts is '${support_zts}', expected 'false' (ZTS is out of the V1 release matrix, plan Task 20)"
ok "support-nts: ${support_nts}, support-zts: ${support_zts}"

echo "==> Checking OS families"
os_families="$(jq -r '.["php-ext"]."os-families" | length' "${ROOT_COMPOSER}")"
[[ "${os_families}" -eq 1 ]] \
    || fail "os-families must contain exactly 1 entry, found ${os_families}"
os_family="$(jq -r '.["php-ext"]."os-families"[0]' "${ROOT_COMPOSER}")"
[[ "${os_family}" == "linux" ]] \
    || fail "os-families[0] is '${os_family}', expected 'linux'"
ok "os-families: [${os_family}]"

echo "==> Checking Laravel package metadata"
laravel_name="$(jq -r '.name' "${LARAVEL_COMPOSER}")"
[[ "${laravel_name}" == "goopil/rabbit-rs-laravel" ]] \
    || fail "Laravel package name is '${laravel_name}', expected 'goopil/rabbit-rs-laravel'"
ok "Laravel package name: ${laravel_name}"

laravel_ns="$(jq -r '.autoload."psr-4" | keys[]' "${LARAVEL_COMPOSER}")"
expected_ns="Goopil\\RabbitRs\\Laravel\\"
[[ "${laravel_ns}" == "${expected_ns}" ]] \
    || fail "Laravel namespace is '${laravel_ns}', expected '${expected_ns}'"
ok "Laravel namespace: ${laravel_ns}"

ext_req="$(jq -r '.require."ext-rabbit_rs"' "${LARAVEL_COMPOSER}")"
[[ "${ext_req}" =~ ^\^([0-9]+) ]] \
    || fail "ext-rabbit_rs constraint '${ext_req}' does not pin a major version"
laravel_major="${BASH_REMATCH[1]}"
ok "Laravel requires ext-rabbit_rs: ${ext_req} (major ${laravel_major})"

echo "==> Checking Cargo version"
cargo_version="$(grep -E '^version' "${CARGO_TOML}" | head -1 | sed 's/.*= *"//' | sed 's/"//')"
[[ "${cargo_version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || fail "Cargo version '${cargo_version}' is not semver X.Y.Z"
ok "Cargo version: ${cargo_version}"

cargo_major="${cargo_version%%.*}"

echo "==> Checking version coherence"
[[ "${cargo_major}" == "${laravel_major}" ]] \
    || fail "Cargo major ${cargo_major} != Laravel ext-rabbit_rs major ${laravel_major}"
ok "major versions match: ${cargo_major}"

echo "==> Checking PIE matrix"
matrix_count="$(jq -r '.matrix | length' "${PIE_MATRIX}")"
[[ "${matrix_count}" -eq 8 ]] \
    || fail "PIE matrix must have exactly 8 entries, found ${matrix_count}"
ok "PIE matrix entries: ${matrix_count}"

echo "==> Checking PIE matrix uniqueness and values"
php_values="$(jq -r '.matrix[].php' "${PIE_MATRIX}" | sort -u)"
arch_values="$(jq -r '.matrix[].arch' "${PIE_MATRIX}" | sort -u)"
libc_values="$(jq -r '.matrix[].libc' "${PIE_MATRIX}" | sort -u)"
ts_values="$(jq -r '.matrix[].thread_safety' "${PIE_MATRIX}" | sort -u)"

expected_php="8.4
8.5"
expected_arch="arm64
x86_64"
expected_libc="glibc
musl"
expected_ts="nts"

[[ "${php_values}" == "${expected_php}" ]] \
    || fail "PHP versions are ${php_values//$'\n'/, }, expected 8.4,8.5"
[[ "${arch_values}" == "${expected_arch}" ]] \
    || fail "architectures are ${arch_values//$'\n'/, }, expected arm64,x86_64"
[[ "${libc_values}" == "${expected_libc}" ]] \
    || fail "libc values are ${libc_values//$'\n'/, }, expected glibc,musl"
[[ "${ts_values}" == "${expected_ts}" ]] \
    || fail "thread_safety values are ${ts_values//$'\n'/, }, expected nts (ZTS excluded in V1)"
ok "PIE matrix values are correct"

unique_suffixes="$(jq -r '.matrix[].artifact_suffix' "${PIE_MATRIX}" | sort -u | wc -l | tr -d ' ')"
[[ "${unique_suffixes}" -eq 8 ]] \
    || fail "artifact_suffix entries must all be unique, found ${unique_suffixes} unique out of 8"
ok "All 8 artifact suffixes are unique"

echo "==> Checking glibc minimum"
min_glibc="$(jq -r '.minimum_glibc' "${PIE_MATRIX}")"
[[ "${min_glibc}" =~ ^[0-9]+\.[0-9]+$ ]] \
    || fail "minimum_glibc '${min_glibc}' is not a valid version"
ok "minimum_glibc: ${min_glibc}"

echo "==> Checking packaging scripts"
need_file "${ROOT_DIR}/scripts/package-pie-binary.sh"
need_file "${ROOT_DIR}/scripts/split-laravel-package.sh"
[[ -x "${ROOT_DIR}/scripts/package-pie-binary.sh" ]] \
    || fail "scripts/package-pie-binary.sh is not executable"
[[ -x "${ROOT_DIR}/scripts/split-laravel-package.sh" ]] \
    || fail "scripts/split-laravel-package.sh is not executable"
ok "packaging scripts present and executable"

echo ""
echo "All distribution checks passed."
