#!/usr/bin/env bash
set -euo pipefail

# verify-release-assets.sh — verify release artifacts before publishing.
#
# Checks that exactly 16 PIE archives exist, each with a SHA-256 checksum.
# Rejects debug artifacts and unsupported platforms.
# Validates version synchronization across Cargo, extension, and tag.
#
# Usage:
#   ./scripts/verify-release-assets.sh --fixtures release/pie-matrix.json
#   ./scripts/verify-release-assets.sh --fixtures release/pie-matrix.json --release-dir release/
#   ./scripts/verify-release-assets.sh --fixtures release/pie-matrix.json --version 1.0.0

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

FIXTURES=""
RELEASE_DIR="${ROOT_DIR}/release"
VERSION=""
EXPECTED_ARCHIVE_COUNT=16

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

usage() {
    cat <<'USAGE'
Usage: verify-release-assets.sh [options]

Options:
  --fixtures <file>     PIE matrix JSON file (required)
  --release-dir <dir>   Directory containing archives (default: release/)
  --version <ver>       Expected semver version (default: read from Cargo.toml)
  -h, --help            Show this help
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --fixtures)    FIXTURES="$2"; shift 2 ;;
        --release-dir) RELEASE_DIR="$2"; shift 2 ;;
        --version)     VERSION="$2"; shift 2 ;;
        -h|--help)     usage; exit 0 ;;
        *)             fail "unknown argument: $1" ;;
    esac
done

[[ -n "${FIXTURES}" ]] || { usage; fail "--fixtures is required"; }

if [[ ! -f "${FIXTURES}" ]]; then
    fail "fixtures file not found: ${FIXTURES}"
fi

if ! command -v jq >/dev/null 2>&1; then
    fail "jq is required"
fi

if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "sha256sum or shasum is required"
fi

sha256_func() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}

# --- Determine version --------------------------------------------------------

if [[ -z "${VERSION}" ]]; then
    CARGO_TOML="${ROOT_DIR}/Cargo.toml"
    if [[ ! -f "${CARGO_TOML}" ]]; then
        fail "Cargo.toml not found at ${CARGO_TOML} — specify --version"
    fi
    VERSION="$(grep -E '^version' "${CARGO_TOML}" | head -1 | sed 's/.*= *"//' | sed 's/"//')"
fi

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    fail "version '${VERSION}' is not semver X.Y.Z"
fi

version_lower="$(echo "${VERSION}" | tr '[:upper:]' '[:lower:]')"
if [[ "${version_lower}" == *"debug"* || "${version_lower}" == *"dev"* || "${version_lower}" == *"snapshot"* ]]; then
    fail "debug/dev/snapshot versions are not distributable: ${VERSION}"
fi

ok "version: ${VERSION}"

# --- Read and validate the PIE matrix ----------------------------------------

echo "==> Reading PIE matrix: ${FIXTURES}"

matrix_count="$(jq -r '.matrix | length' "${FIXTURES}")"
if [[ "${matrix_count}" -ne "${EXPECTED_ARCHIVE_COUNT}" ]]; then
    fail "PIE matrix must have exactly ${EXPECTED_ARCHIVE_COUNT} entries, found ${matrix_count}"
fi
ok "PIE matrix has ${matrix_count} entries"

php_ext_name="$(jq -r '.php_extension_name' "${FIXTURES}")"
if [[ "${php_ext_name}" != "rabbit_rs" ]]; then
    fail "php_extension_name is '${php_ext_name}', expected 'rabbit_rs'"
fi
ok "extension name: ${php_ext_name}"

# --- Validate each matrix entry and expected artifact names -------------------

echo "==> Validating artifact names against PIE naming convention"

declare -a expected_archives

for i in $(seq 0 $((matrix_count - 1))); do
    entry_php="$(jq -r ".matrix[${i}].php" "${FIXTURES}")"
    entry_arch="$(jq -r ".matrix[${i}].arch" "${FIXTURES}")"
    entry_libc="$(jq -r ".matrix[${i}].libc" "${FIXTURES}")"
    entry_ts="$(jq -r ".matrix[${i}].thread_safety" "${FIXTURES}")"
    entry_ts_suffix="$(jq -r ".matrix[${i}].ts_suffix" "${FIXTURES}")"

    # Validate PHP version
    case "${entry_php}" in
        8.4|8.5) : ;;
        *) fail "unsupported PHP version in matrix entry ${i}: ${entry_php}" ;;
    esac

    # Validate architecture
    case "${entry_arch}" in
        x86_64|arm64) : ;;
        *) fail "unsupported architecture in matrix entry ${i}: ${entry_arch}" ;;
    esac

    # Validate libc
    case "${entry_libc}" in
        glibc|musl) : ;;
        *) fail "unsupported libc in matrix entry ${i}: ${entry_libc}" ;;
    esac

    # Validate thread safety
    case "${entry_ts}" in
        nts|zts) : ;;
        *) fail "unsupported thread_safety in matrix entry ${i}: ${entry_ts}" ;;
    esac

    # Build expected archive name: php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}{ts_suffix}.zip
    expected_name="php_${php_ext_name}-v${VERSION}_php${entry_php}-${entry_arch}-linux-${entry_libc}${entry_ts_suffix}.zip"
    expected_archives+=("${expected_name}")
done

# Check uniqueness
unique_count="$(printf '%s\n' "${expected_archives[@]}" | sort -u | wc -l | tr -d ' ')"
if [[ "${unique_count}" -ne "${EXPECTED_ARCHIVE_COUNT}" ]]; then
    fail "expected ${EXPECTED_ARCHIVE_COUNT} unique archive names, found ${unique_count}"
fi
ok "all ${EXPECTED_ARCHIVE_COUNT} archive names are unique and follow PIE naming convention"

# --- Check that archives exist with SHA-256 ----------------------------------

echo "==> Checking release artifacts in ${RELEASE_DIR}"

if [[ ! -d "${RELEASE_DIR}" ]]; then
    fail "release directory not found: ${RELEASE_DIR}"
fi

found_archives=0
missing_archives=()

for archive in "${expected_archives[@]}"; do
    archive_path="${RELEASE_DIR}/${archive}"
    sha_path="${archive_path}.sha256"

    if [[ ! -f "${archive_path}" ]]; then
        missing_archives+=("${archive}")
        continue
    fi

    found_archives=$((found_archives + 1))

    # Check SHA-256 file
    if [[ ! -f "${sha_path}" ]]; then
        fail "missing SHA-256 file: ${sha_path}"
    fi

    # Verify SHA-256 content matches the archive
    stored_sha="$(cat "${sha_path}" | awk '{print $1}')"
    actual_sha="$(sha256_func "${archive_path}")"
    if [[ "${stored_sha}" != "${actual_sha}" ]]; then
        fail "SHA-256 mismatch for ${archive}: stored=${stored_sha} actual=${actual_sha}"
    fi

    # Check SBOM file
    archive_base="${archive%.zip}"
    sbom_path="${RELEASE_DIR}/${archive_base}.sbom.json"
    if [[ ! -f "${sbom_path}" ]]; then
        fail "missing SBOM file: ${sbom_path}"
    fi

    # Validate SBOM is JSON and CycloneDX
    if ! jq -e '.bomFormat == "CycloneDX" and .specVersion == "1.5"' \
            "${sbom_path}" >/dev/null 2>&1; then
        fail "invalid CycloneDX SBOM: ${sbom_path}"
    fi

    ok "artifact verified: ${archive} (+sha256 +sbom)"
done

if [[ "${found_archives}" -ne "${EXPECTED_ARCHIVE_COUNT}" ]]; then
    fail "expected ${EXPECTED_ARCHIVE_COUNT} archives, found ${found_archives}. Missing: ${missing_archives[*]}"
fi
ok "all ${EXPECTED_ARCHIVE_COUNT} archives present with SHA-256"

# --- Reject debug artifacts and unsupported platforms -------------------------

echo "==> Scanning for debug artifacts and unsupported platforms"

all_zips=()
while IFS= read -r f; do
    all_zips+=("$(basename "${f}")")
done < <(find "${RELEASE_DIR}" -maxdepth 1 -name '*.zip' -type f 2>/dev/null || true)

for zip in "${all_zips[@]}"; do
    found=0
    for expected in "${expected_archives[@]}"; do
        if [[ "${zip}" == "${expected}" ]]; then
            found=1
            break
        fi
    done
    if [[ "${found}" -eq 0 ]]; then
        fail "unexpected archive in release dir: ${zip} — not in PIE matrix"
    fi
done

all_sboms=()
while IFS= read -r f; do
    all_sboms+=("$(basename "${f}")")
done < <(find "${RELEASE_DIR}" -maxdepth 1 -name '*.sbom.json' -type f 2>/dev/null || true)

for sbom in "${all_sboms[@]}"; do
    found=0
    for expected in "${expected_archives[@]}"; do
        expected_sbom="${expected%.zip}.sbom.json"
        if [[ "${sbom}" == "${expected_sbom}" ]]; then
            found=1
            break
        fi
    done
    if [[ "${found}" -eq 0 ]]; then
        fail "unexpected SBOM in release dir: ${sbom} — not in PIE matrix"
    fi
done

for zip in "${all_zips[@]}"; do
    zip_lower="$(echo "${zip}" | tr '[:upper:]' '[:lower:]')"
    if [[ "${zip_lower}" == *"debug"* ]]; then
        fail "debug artifact found in release dir: ${zip}"
    fi
    if [[ "${zip_lower}" == *"dev"* ]]; then
        fail "dev artifact found in release dir: ${zip}"
    fi
done

for zip in "${all_zips[@]}"; do
    if [[ "${zip}" == *"darwin"* || "${zip}" == *"macos"* || "${zip}" == *"windows"* || "${zip}" == *"win"* ]]; then
        fail "unsupported platform artifact: ${zip} — only linux is supported"
    fi
done

ok "no debug artifacts or unsupported platform artifacts"

# --- Validate version synchronization -----------------------------------------

echo "==> Validating version synchronization"

CARGO_TOML="${ROOT_DIR}/Cargo.toml"
if [[ -f "${CARGO_TOML}" ]]; then
    cargo_version="$(grep -E '^version' "${CARGO_TOML}" | head -1 | sed 's/.*= *"//' | sed 's/"//')"
    if [[ "${cargo_version}" != "${VERSION}" ]]; then
        fail "Cargo.toml version '${cargo_version}' does not match expected '${VERSION}'"
    fi
    ok "Cargo.toml version: ${cargo_version}"
fi

ROOT_COMPOSER="${ROOT_DIR}/composer.json"
if [[ -f "${ROOT_COMPOSER}" ]]; then
    root_name="$(jq -r '.name' "${ROOT_COMPOSER}")"
    if [[ "${root_name}" != "goopil/rabbit-rs-native" ]]; then
        fail "root composer.json name is '${root_name}', expected 'goopil/rabbit-rs-native'"
    fi
    ok "root composer.json name: ${root_name}"
fi

LARAVEL_COMPOSER="${ROOT_DIR}/packages/laravel-queue/composer.json"
if [[ -f "${LARAVEL_COMPOSER}" ]]; then
    ext_req="$(jq -r '.require."ext-rabbit_rs"' "${LARAVEL_COMPOSER}")"
    if [[ ! "${ext_req}" =~ \^([0-9]+) ]]; then
        fail "Laravel ext-rabbit_rs constraint '${ext_req}' does not pin a major version"
    fi
    laravel_major="${BASH_REMATCH[1]}"
    expected_major="${VERSION%%.*}"
    if [[ "${laravel_major}" != "${expected_major}" ]]; then
        fail "Laravel ext-rabbit_rs major ${laravel_major} does not match version major ${expected_major}"
    fi
    ok "Laravel ext-rabbit_rs constraint: ${ext_req} (major ${laravel_major} matches version major ${expected_major})"
fi

if [[ -n "${GITHUB_REF:-}" ]]; then
    tag_name="${GITHUB_REF#refs/tags/}"
    if [[ "${tag_name}" == v* ]]; then
        tag_version="${tag_name#v}"
        if [[ "${tag_version}" != "${VERSION}" ]]; then
            fail "git tag '${tag_name}' version '${tag_version}' does not match expected '${VERSION}'"
        fi
        ok "git tag version: ${tag_version} (matches)"
    fi
fi

echo ""
echo "All release artifact checks passed."
echo "  Version: ${VERSION}"
echo "  Archives: ${found_archives}/${EXPECTED_ARCHIVE_COUNT}"
echo "  SHA-256: verified"
echo "  SBOM (CycloneDX): verified"
echo "  No debug artifacts"
echo "  No unsupported platforms"
