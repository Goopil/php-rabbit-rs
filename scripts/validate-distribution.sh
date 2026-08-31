#!/usr/bin/env bash
set -euo pipefail

# validate-distribution.sh — verify release readiness in one pass.
#
# Merges the former validate-distribution.sh, verify-pie-naming.sh, and
# verify-release-assets.sh. Checks:
#   1. Package metadata, PIE matrix, and version coherence.
#   2. PIE 1.5.x pre-packaged binary naming convention, kept consistent
#      across the packaging script, the release workflow, and the docs.
#   3. Release artifacts, when the release directory contains archives:
#      exactly the 30 expected files (10 ZIP + 10 SHA-256 + 10 SBOM for the
#      8-entry Linux matrix plus 2 macOS darwin assets), checksum and SBOM
#      validation, and version synchronization up to the git tag.
#
# Exits non-zero if any check fails. Run before tagging a release or
# publishing the extension / Laravel package to Packagist.
#
# Usage:
#   ./scripts/validate-distribution.sh [--release-dir <dir>] [--version <ver>] [-h]
#
#   --release-dir <dir>  Directory containing archives (default: release/)
#   --version <ver>      Expected semver version (default: read from Cargo.toml)

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib-release.sh
source "${ROOT_DIR}/scripts/lib-release.sh"

ROOT_COMPOSER="${ROOT_DIR}/composer.json"
LARAVEL_COMPOSER="${ROOT_DIR}/packages/laravel-queue/composer.json"
CARGO_TOML="${ROOT_DIR}/Cargo.toml"
PIE_MATRIX="${ROOT_DIR}/release/pie-matrix.json"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
DOCS="${ROOT_DIR}/docs/distribution.md"
PKG_SCRIPT="${ROOT_DIR}/scripts/package-pie-binary.sh"
SPLIT_SCRIPT="${ROOT_DIR}/scripts/split-laravel-package.sh"
EXPECTED_LINUX_ARCHIVE_COUNT=8

cargo_version() {
    grep -E '^version' "${CARGO_TOML}" | head -1 | sed 's/.*= *"//' | sed 's/"//'
}

usage() {
    cat <<'USAGE'
Usage: validate-distribution.sh [options]

Options:
  --release-dir <dir>  Directory containing archives (default: release/)
  --version <ver>      Expected semver version (default: read from Cargo.toml)
  -h, --help           Show this help
USAGE
}

RELEASE_DIR="${ROOT_DIR}/release"
VERSION=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release-dir) RELEASE_DIR="$2"; shift 2 ;;
        --version)     VERSION="$2"; shift 2 ;;
        -h|--help)     usage; exit 0 ;;
        *)             fail "unknown argument: $1" ;;
    esac
done

echo "==> Checking required files and tools"
need_file "${ROOT_COMPOSER}"
need_file "${LARAVEL_COMPOSER}"
need_file "${CARGO_TOML}"
need_file "${PIE_MATRIX}"
need_file "${WORKFLOW}"
need_file "${DOCS}"
need_file "${PKG_SCRIPT}"
need_file "${SPLIT_SCRIPT}"
[[ -x "${PKG_SCRIPT}" ]] || fail "scripts/package-pie-binary.sh is not executable"
[[ -x "${SPLIT_SCRIPT}" ]] || fail "scripts/split-laravel-package.sh is not executable"
need_cmd jq
need_cmd grep
ok "required files and tools present"

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
cargo_ver="$(cargo_version)"
[[ "${cargo_ver}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || fail "Cargo version '${cargo_ver}' is not semver X.Y.Z"
ok "Cargo version: ${cargo_ver}"

cargo_major="${cargo_ver%%.*}"

echo "==> Checking version coherence"
[[ "${cargo_major}" == "${laravel_major}" ]] \
    || fail "Cargo major ${cargo_major} != Laravel ext-rabbit_rs major ${laravel_major}"
ok "major versions match: ${cargo_major}"

echo "==> Checking PIE matrix"
matrix_count="$(jq -r '.matrix | length' "${PIE_MATRIX}")"
[[ "${matrix_count}" -eq "${EXPECTED_LINUX_ARCHIVE_COUNT}" ]] \
    || fail "PIE matrix must have exactly ${EXPECTED_LINUX_ARCHIVE_COUNT} entries, found ${matrix_count}"
ok "PIE matrix entries: ${matrix_count}"

php_ext_name="$(jq -r '.php_extension_name' "${PIE_MATRIX}")"
[[ "${php_ext_name}" == "rabbit_rs" ]] \
    || fail "php_extension_name is '${php_ext_name}', expected 'rabbit_rs'"

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
[[ "${unique_suffixes}" -eq "${EXPECTED_LINUX_ARCHIVE_COUNT}" ]] \
    || fail "artifact_suffix entries must all be unique, found ${unique_suffixes} unique out of ${EXPECTED_LINUX_ARCHIVE_COUNT}"
ok "All ${EXPECTED_LINUX_ARCHIVE_COUNT} artifact suffixes are unique"

echo "==> Checking glibc minimum"
min_glibc="$(jq -r '.minimum_glibc' "${PIE_MATRIX}")"
[[ "${min_glibc}" =~ ^[0-9]+\.[0-9]+$ ]] \
    || fail "minimum_glibc '${min_glibc}' is not a valid version"
ok "minimum_glibc: ${min_glibc}"

# --- PIE naming convention (former verify-pie-naming.sh) ----------------------
#
# PIE accepts NTS asset names with or without an explicit -nts suffix, but
# requires the -zts suffix for ZTS builds. The Rabbit RS convention is that
# every Linux artifact carries an explicit thread-safety suffix (-nts) so
# that asset names are unambiguous and self-describing. ZTS is planned for
# V2 with TSRM isolation (plan Task 20, Option A).

echo "==> Checking ZTS exclusion (V1 decision)"
if [[ "${support_zts}" != "true" ]]; then
    if grep -qi 'zts' "${PIE_MATRIX}"; then
        fail "zts entries found in pie-matrix.json after ZTS removal (Task 20)"
    fi
fi
ok "no zts entries in the PIE matrix (support-zts: ${support_zts})"

echo "==> Checking PIE asset naming"

# PIE candidate names for Linux pre-packaged binaries, no debug builds,
# .zip format. Mirrors php/pie 1.5.x PrePackagedBinaryAssetName::packageNames:
# the version component is the Composer pretty version (v-prefixed tag), and
# Linux additionally accepts an anylibc fallback flavour.
pie_candidates() {
    local php="$1" arch="$2" libc="$3" ts="$4"
    local ts_modes=()
    if [[ "${ts}" == "zts" ]]; then
        ts_modes=("-zts")
    else
        ts_modes=("" "-nts")
    fi
    local flavour suffix
    for flavour in "${libc}" "anylibc"; do
        for suffix in "${ts_modes[@]}"; do
            printf 'php_%s-v1.2.3_php%s-%s-linux-%s%s.zip\n' \
                "${php_ext_name}" "${php}" "${arch}" "${flavour}" "${suffix}"
            printf 'php_%s-v1.2.3_php%s-%s-linux-%s%s.tgz\n' \
                "${php_ext_name}" "${php}" "${arch}" "${flavour}" "${suffix}"
        done
    done
}

declare -a linux_archives
for i in $(seq 0 $((matrix_count - 1))); do
    entry_php="$(jq -r ".matrix[${i}].php" "${PIE_MATRIX}")"
    entry_arch="$(jq -r ".matrix[${i}].arch" "${PIE_MATRIX}")"
    entry_libc="$(jq -r ".matrix[${i}].libc" "${PIE_MATRIX}")"
    entry_ts="$(jq -r ".matrix[${i}].thread_safety" "${PIE_MATRIX}")"
    entry_ts_suffix="$(jq -r ".matrix[${i}].ts_suffix" "${PIE_MATRIX}")"

    # Unification invariant: the suffix must be explicit for every entry
    # (previously NTS entries carried an empty suffix, which made the
    # workflow and the packaging script disagree).
    [[ "${entry_ts_suffix}" == "-${entry_ts}" ]] \
        || fail "matrix entry ${i} (${entry_php}/${entry_arch}/${entry_libc}/${entry_ts}): ts_suffix is '${entry_ts_suffix}', expected '-${entry_ts}'"

    name="$(jq -r ".matrix[${i}].artifact_suffix" "${PIE_MATRIX}")"
    expected_name="php${entry_php}-${entry_arch}-linux-${entry_libc}${entry_ts_suffix}"
    [[ "${name}" == "${expected_name}" ]] \
        || fail "matrix entry ${i}: artifact_suffix is '${name}', expected '${expected_name}'"

    archive="php_${php_ext_name}-v1.2.3_${name}.zip"
    if ! pie_candidates "${entry_php}" "${entry_arch}" "${entry_libc}" "${entry_ts}" | grep -qx -- "${archive}"; then
        fail "asset name '${archive}' is not an accepted PIE pre-packaged binary name"
    fi

    linux_archives+=("${archive}")
done
ok "all ${matrix_count} asset names carry an explicit thread-safety suffix and match the PIE naming pattern"

unique_names="$(printf '%s\n' "${linux_archives[@]}" | sort -u | wc -l | tr -d ' ')"
[[ "${unique_names}" -eq "${matrix_count}" ]] \
    || fail "expected ${matrix_count} unique asset names, found ${unique_names}"
ok "all ${matrix_count} asset names are unique"

echo "==> Checking release workflow naming"

# The Linux build job must append the explicit thread-safety suffix from the
# matrix variable, never an implicit empty suffix for NTS.
grep -q -- 'php_rabbit_rs-v${{ needs.create-release.outputs.version }}_php${{ matrix.php }}-${{ matrix.arch }}-linux-${{ matrix.libc }}-${{ matrix.ts }}' "${WORKFLOW}" \
    || fail "release.yml build-linux asset-base does not produce the unified naming pattern (explicit -\${{ matrix.ts }} suffix)"

# macOS artifacts are outside the PIE matrix (os-families: linux) and are
# consumed by Homebrew; they keep the explicit -nts suffix.
grep -q 'arm64-darwin-nts' "${WORKFLOW}" \
    || fail "release.yml macOS Package step lost the explicit -nts suffix"
ok "release workflow produces the unified naming pattern"

echo "==> Checking documented convention"
grep -q 'php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip' "${DOCS}" \
    || fail "docs/distribution.md does not document the unified naming pattern"
ok "docs/distribution.md documents the unified naming pattern"

# --- Release artifacts (former verify-release-assets.sh) ----------------------

echo "==> Resolving expected version"
if [[ -z "${VERSION}" ]]; then
    VERSION="${cargo_ver}"
fi
if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    fail "version '${VERSION}' is not semver X.Y.Z"
fi

version_lower="$(echo "${VERSION}" | tr '[:upper:]' '[:lower:]')"
if [[ "${version_lower}" == *"debug"* || "${version_lower}" == *"dev"* || "${version_lower}" == *"snapshot"* ]]; then
    fail "debug/dev/snapshot versions are not distributable: ${VERSION}"
fi
ok "version: ${VERSION}"

[[ "${cargo_ver}" == "${VERSION}" ]] \
    || fail "Cargo.toml version '${cargo_ver}' does not match expected '${VERSION}'"
ok "Cargo.toml version: ${cargo_ver}"

if [[ -n "${GITHUB_REF:-}" ]]; then
    tag_name="${GITHUB_REF#refs/tags/}"
    if [[ "${tag_name}" == v* ]]; then
        tag_version="${tag_name#v}"
        [[ "${tag_version}" == "${VERSION}" ]] \
            || fail "git tag '${tag_name}' version '${tag_version}' does not match expected '${VERSION}'"
        ok "git tag version: ${tag_version} (matches)"
    fi
fi

# Expected inventory: the 8 Linux matrix ZIPs plus 2 macOS darwin ZIPs
# (one per matrix PHP version, arm64-darwin-nts), each with .sha256 and
# .sbom.json sidecars — 30 files in total, matching the release workflow.
declare -a expected_bases
for i in $(seq 0 $((matrix_count - 1))); do
    suffix="$(jq -r ".matrix[${i}].artifact_suffix" "${PIE_MATRIX}")"
    expected_bases+=("php_${php_ext_name}-v${VERSION}_${suffix}")
done
while IFS= read -r matrix_php; do
    expected_bases+=("php_${php_ext_name}-v${VERSION}_php${matrix_php}-arm64-darwin-nts")
done < <(jq -r '.matrix[].php' "${PIE_MATRIX}" | sort -u)

declare -a expected_files
for base in "${expected_bases[@]}"; do
    expected_files+=("${base}.zip" "${base}.zip.sha256" "${base}.sbom.json")
done
expected_total=${#expected_files[@]}

if [[ ! -d "${RELEASE_DIR}" ]]; then
    if [[ "${RELEASE_DIR}" != "${ROOT_DIR}/release" ]]; then
        fail "release directory not found: ${RELEASE_DIR}"
    fi
    echo "==> Skipping artifact verification"
    echo "  ${RELEASE_DIR} does not exist — package artifacts with"
    echo "  scripts/package-pie-binary.sh and assemble the release directory to verify."
    echo ""
    echo "All distribution checks passed (no artifacts to verify)."
    exit 0
fi

echo "==> Checking release artifacts in ${RELEASE_DIR}"
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "sha256sum or shasum is required"
fi

found_files=0
unexpected_files=()
while IFS= read -r f; do
    base="$(basename "${f}")"
    case "${base}" in
        *.zip|*.zip.sha256|*.sbom.json) ;;
        *) continue ;;
    esac
    found_files=$((found_files + 1))
    matched=0
    for expected in "${expected_files[@]}"; do
        if [[ "${base}" == "${expected}" ]]; then
            matched=1
            break
        fi
    done
    if [[ "${matched}" -eq 0 ]]; then
        unexpected_files+=("${base}")
    fi
done < <(find "${RELEASE_DIR}" -maxdepth 1 -type f \( -name '*.zip' -o -name '*.zip.sha256' -o -name '*.sbom.json' \) 2>/dev/null || true)

if [[ "${found_files}" -eq 0 ]]; then
    if [[ "${RELEASE_DIR}" != "${ROOT_DIR}/release" ]]; then
        fail "no artifacts found in ${RELEASE_DIR}"
    fi
    echo "==> Skipping artifact verification"
    echo "  ${RELEASE_DIR} contains no archives — package artifacts with"
    echo "  scripts/package-pie-binary.sh and assemble the release directory to verify."
    echo ""
    echo "All distribution checks passed (no artifacts to verify)."
    exit 0
fi

if [[ ${#unexpected_files[@]} -gt 0 ]]; then
    fail "unexpected file(s) in release dir: ${unexpected_files[*]}"
fi
if [[ "${found_files}" -ne "${expected_total}" ]]; then
    fail "expected exactly ${expected_total} files (10 ZIP + 10 SHA256 + 10 SBOM), found ${found_files}"
fi
ok "all ${expected_total} expected files present (10 ZIP + 10 SHA256 + 10 SBOM), no extras"

for base in "${expected_bases[@]}"; do
    archive_path="${RELEASE_DIR}/${base}.zip"
    sha_path="${archive_path}.sha256"
    sbom_path="${RELEASE_DIR}/${base}.sbom.json"

    stored_sha="$(awk '{print $1}' "${sha_path}")"
    actual_sha="$(sha256_func "${archive_path}")"
    [[ "${stored_sha}" == "${actual_sha}" ]] \
        || fail "SHA-256 mismatch for ${base}.zip: stored=${stored_sha} actual=${actual_sha}"

    if ! jq -e '.bomFormat == "CycloneDX" and .specVersion == "1.5"' \
            "${sbom_path}" >/dev/null 2>&1; then
        fail "invalid CycloneDX SBOM: ${sbom_path}"
    fi

    ok "artifact verified: ${base}.zip (+sha256 +sbom)"
done

echo ""
echo "All distribution checks passed."
echo "  Version: ${VERSION}"
echo "  Files: ${found_files}/${expected_total}"
echo "  SHA-256: verified"
echo "  SBOM (CycloneDX): verified"
