#!/usr/bin/env bash
set -euo pipefail

# package-pie-binary.sh — package a single PIE release artifact.
#
# V1 ships NTS binaries only (plan Task 20, Option A): --ts accepts nts and
# rejects zts until TSRM isolation lands in V2. See docs/distribution.md.
#
# Usage:
#   ./scripts/package-pie-binary.sh \
#       --version 1.2.0 \
#       --php 8.5 \
#       --arch x86_64 \
#       --libc glibc \
#       --ts nts \
#       --so /path/to/rabbit_rs.so
#
# Produces:
#   php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip
#   php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip.sha256
#   php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.sbom.json
#
# V1 matrix: 8 artifacts (2 PHP × 2 arch × 2 libc, NTS only).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXTENSION_NAME="rabbit_rs"
EXPECTED_SO_NAME="rabbit_rs.so"

# --- helpers -----------------------------------------------------------------

# shellcheck source=lib-release.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib-release.sh"

usage() {
    cat <<'USAGE'
Usage: package-pie-binary.sh [options]

Options:
  --version <ver>     Extension version (semver X.Y.Z), e.g. 1.2.0
  --php <ver>         PHP major.minor, e.g. 8.4 or 8.5
  --arch <arch>       x86_64 or arm64
  --libc <libc>       glibc or musl
  --ts <mode>         nts (V1 is NTS-only; zts is not distributed)
  --so <path>         Path to the compiled rabbit_rs.so
  -h, --help          Show this help

Example:
  package-pie-binary.sh --version 1.2.0 --php 8.5 --arch x86_64 --libc glibc --ts nts --so ./rabbit_rs.so
USAGE
}

# --- argument parsing --------------------------------------------------------

VERSION=""
PHP_VER=""
ARCH=""
LIBC=""
TS=""
SO_PATH=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)  VERSION="$2"; shift 2 ;;
        --php)      PHP_VER="$2"; shift 2 ;;
        --arch)     ARCH="$2"; shift 2 ;;
        --libc)     LIBC="$2"; shift 2 ;;
        --ts)       TS="$2"; shift 2 ;;
        --so)       SO_PATH="$2"; shift 2 ;;
        -h|--help)  usage; exit 0 ;;
        *)          fail "unknown argument: $1" ;;
    esac
done

# --- validate arguments -------------------------------------------------------

[[ -n "${VERSION}" ]] || { usage; fail "--version is required"; }
[[ -n "${PHP_VER}" ]] || { usage; fail "--php is required"; }
[[ -n "${ARCH}" ]] || { usage; fail "--arch is required"; }
[[ -n "${LIBC}" ]] || { usage; fail "--libc is required"; }
[[ -n "${TS}" ]] || { usage; fail "--ts is required"; }
[[ -n "${SO_PATH}" ]] || { usage; fail "--so is required"; }

# --- validate version (semver, no debug) -------------------------------------

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    fail "version '${VERSION}' is not semver X.Y.Z"
fi

version_lower="$(echo "${VERSION}" | tr '[:upper:]' '[:lower:]')"
if [[ "${version_lower}" == *"debug"* || "${version_lower}" == *"dev"* ]]; then
    fail "debug/dev builds are not distributable: ${VERSION}"
fi

# --- validate PHP version -----------------------------------------------------

if [[ ! "${PHP_VER}" =~ ^[0-9]+\.[0-9]+$ ]]; then
    fail "PHP version '${PHP_VER}' is not major.minor"
fi

case "${PHP_VER}" in
    8.4|8.5) : ;;
    *) fail "unsupported PHP version: ${PHP_VER} (expected 8.4 or 8.5)" ;;
esac

# --- validate architecture ----------------------------------------------------

case "${ARCH}" in
    x86_64|arm64) : ;;
    *) fail "unsupported architecture: ${ARCH} (expected x86_64 or arm64)" ;;
esac

# --- validate libc ------------------------------------------------------------

case "${LIBC}" in
    glibc|musl) : ;;
    *) fail "unsupported libc: ${LIBC} (expected glibc or musl)" ;;
esac

# --- validate thread safety --------------------------------------------------

case "${TS}" in
    nts) : ;;
    zts) fail "unsupported thread-safety: zts (V1 is NTS-only, plan Task 20 — ZTS is planned for V2)" ;;
    *) fail "unsupported thread-safety: ${TS} (expected nts)" ;;
esac

# --- validate shared object path ---------------------------------------------

if [[ ! -f "${SO_PATH}" ]]; then
    fail "shared object not found: ${SO_PATH}"
fi

so_basename="$(basename "${SO_PATH}")"
if [[ "${so_basename}" != "${EXPECTED_SO_NAME}" ]]; then
    fail "shared object must be named '${EXPECTED_SO_NAME}', got '${so_basename}'"
fi

if [[ $(uname -s) == "Darwin" ]]; then
    file_output="$(file "${SO_PATH}")"
    if [[ "${file_output}" == *"Mach-O"* ]]; then
        fail "shared object is a Mach-O dylib, not a Linux ELF shared object: ${SO_PATH}"
    fi
elif [[ $(uname -s) == "Linux" ]]; then
    file_output="$(file "${SO_PATH}")"
    if [[ "${file_output}" != *"ELF"* ]] || [[ "${file_output}" != *"shared object"* ]]; then
        fail "shared object is not an ELF shared object: ${SO_PATH}"
    fi
    if [[ "${file_output}" == *"x86-64"* ]] && [[ "${ARCH}" != "x86_64" ]]; then
        fail "shared object is x86-64 but target arch is ${ARCH}"
    fi
    if [[ "${file_output}" == *"aarch64"* ]] && [[ "${ARCH}" != "arm64" ]]; then
        fail "shared object is aarch64 but target arch is ${ARCH}"
    fi
fi

# --- check for undocumented dynamic libraries ---------------------------------

# On Linux, inspect the .so with `ldd` if available. The archive must contain
# only rabbit_rs.so and no undocumented dynamic libraries. We refuse to embed
# extra .so/.dylib files.
if [[ $(uname -s) == "Linux" ]] && command -v ldd >/dev/null 2>&1; then
    ldd_output="$(ldd "${SO_PATH}" 2>/dev/null || true)"
    # Report but do not fail: glibc/musl are system deps, not bundled.
    if [[ -n "${ldd_output}" ]]; then
        echo "==> Shared object dynamic dependencies (system libraries, not bundled):"
        echo "${ldd_output}" | sed 's/^/    /'
    fi
fi

# --- build archive name -------------------------------------------------------

ARCHIVE_NAME="php_${EXTENSION_NAME}-v${VERSION}_php${PHP_VER}-${ARCH}-linux-${LIBC}-${TS}.zip"
ARCHIVE_BASE="${ARCHIVE_NAME%.zip}"

# Refuse incoherent name: the basename must exactly match the template
expected_base="php_${EXTENSION_NAME}-v${VERSION}_php${PHP_VER}-${ARCH}-linux-${LIBC}-${TS}"
if [[ "${ARCHIVE_BASE}" != "${expected_base}" ]]; then
    fail "archive base '${ARCHIVE_BASE}' does not match expected '${expected_base}'"
fi

ok "archive name: ${ARCHIVE_NAME}"

# --- create ZIP ---------------------------------------------------------------

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

cp "${SO_PATH}" "${WORK_DIR}/${EXPECTED_SO_NAME}"

# Verify only the .so is in the work directory
extra_files="$(find "${WORK_DIR}" -type f ! -name "${EXPECTED_SO_NAME}" -print)"
if [[ -n "${extra_files}" ]]; then
    fail "unexpected files in staging directory: ${extra_files}"
fi

ZIP_PATH="${ROOT_DIR}/release/${ARCHIVE_NAME}"
SHA_PATH="${ZIP_PATH}.sha256"

echo "==> Creating ZIP archive"
( cd "${WORK_DIR}" && zip -j -X "${ZIP_PATH}" "${EXPECTED_SO_NAME}" )
ok "created: ${ZIP_PATH}"

# --- SHA-256 ------------------------------------------------------------------

echo "==> Computing SHA-256"
sha256sum "${ZIP_PATH}" | awk '{print $1}' > "${SHA_PATH}"
ok "created: ${SHA_PATH}"

# --- SBOM (CycloneDX) ---------------------------------------------------------

echo "==> Generating CycloneDX SBOM"
SBOM_PATH="${ROOT_DIR}/release/${ARCHIVE_BASE}.sbom.json"

# Install cargo-cyclonedx if not present (pinned version)
if ! command -v cargo-cyclonedx >/dev/null 2>&1; then
    echo "    installing cargo-cyclonedx 0.5.9"
    cargo install cargo-cyclonedx --version 0.5.9 --locked --quiet
fi

# Generate SBOM from the rabbit-rs-php crate. cargo-cyclonedx writes to
# the crate directory; we move/rename to the release dir with the artifact name.
SBOM_TMP="${ROOT_DIR}/crates/rabbit-rs-php/rabbit-rs-php.cdx.json"
cargo cyclonedx --manifest-path "${ROOT_DIR}/crates/rabbit-rs-php/Cargo.toml" \
    --format json --spec-version 1.5 --quiet
[[ -f "${SBOM_TMP}" ]] \
    || fail "cargo-cyclonedx did not produce ${SBOM_TMP}"
mv "${SBOM_TMP}" "${SBOM_PATH}"

# Validate the SBOM is JSON and looks like CycloneDX
if ! jq -e '.bomFormat == "CycloneDX" and .specVersion == "1.5"' \
        "${SBOM_PATH}" >/dev/null 2>&1; then
    fail "SBOM is not valid CycloneDX JSON: ${SBOM_PATH}"
fi
ok "created: ${SBOM_PATH}"

# --- verify -------------------------------------------------------------------

echo "==> Verifying archive contents"
zip_contents="$(unzip -l "${ZIP_PATH}" | awk 'NR>3 && NF>=4 {print $4}')"
so_found=0
for entry in ${zip_contents}; do
    if [[ "${entry}" == "${EXPECTED_SO_NAME}" ]]; then
        so_found=1
        continue
    fi
    # Ignore empty lines
    if [[ -n "${entry}" ]]; then
        fail "archive contains undocumented file: ${entry}"
    fi
done
[[ "${so_found}" -eq 1 ]] || fail "archive does not contain ${EXPECTED_SO_NAME}"
ok "archive contains only ${EXPECTED_SO_NAME}"

echo ""
echo "Packaging complete:"
echo "  ZIP:    ${ZIP_PATH}"
echo "  SHA256: ${SHA_PATH}"
echo "  SBOM:   ${SBOM_PATH}"
