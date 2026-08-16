#!/usr/bin/env bash
set -euo pipefail

# split-laravel-package.sh — extract packages/laravel-queue into a standalone
# directory suitable for a separate Packagist submission.
#
# Usage:
#   ./scripts/split-laravel-package.sh --output /tmp/laravel-split
#   ./scripts/split-laravel-package.sh --dry-run
#
# The split result keeps packages/laravel-queue/composer.json at the root of
# the output directory and copies src/ and tests/ alongside it.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LARAVEL_PKG_DIR="${ROOT_DIR}/packages/laravel-queue"
ROOT_COMPOSER="${ROOT_DIR}/composer.json"
CARGO_TOML="${ROOT_DIR}/Cargo.toml"

# --- helpers ------------------------------------------------------------------

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

usage() {
    cat <<'USAGE'
Usage: split-laravel-package.sh [options]

Options:
  --output <dir>   Output directory for the split package
  --dry-run        Print what would be done without creating files
  -h, --help       Show this help
USAGE
}

# --- argument parsing ---------------------------------------------------------

OUTPUT_DIR=""
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)  OUTPUT_DIR="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *)         fail "unknown argument: $1" ;;
    esac
done

# --- pre-flight checks --------------------------------------------------------

if [[ ! -d "${LARAVEL_PKG_DIR}" ]]; then
    fail "Laravel package directory not found: ${LARAVEL_PKG_DIR}"
fi

LARAVEL_COMPOSER="${LARAVEL_PKG_DIR}/composer.json"
if [[ ! -f "${LARAVEL_COMPOSER}" ]]; then
    fail "Laravel composer.json not found: ${LARAVEL_COMPOSER}"
fi

need_cmd() {
    local cmd="$1"
    if ! command -v "${cmd}" >/dev/null 2>&1; then
        fail "required command not found: ${cmd}"
    fi
}

need_cmd jq
need_cmd grep

# --- check version compatibility ----------------------------------------------

echo "==> Checking ext-rabbit_rs major version compatibility"

ext_req="$(jq -r '.require."ext-rabbit_rs"' "${LARAVEL_COMPOSER}")"
if [[ ! "${ext_req}" =~ \^([0-9]+) ]]; then
    fail "Laravel composer.json ext-rabbit_rs constraint '${ext_req}' does not pin a major version"
fi
laravel_major="${BASH_REMATCH[1]}"

cargo_version="$(grep -E '^version' "${CARGO_TOML}" | head -1 | sed 's/.*= *"//' | sed 's/"//')"
if [[ ! "${cargo_version}" =~ ^([0-9]+)\.[0-9]+\.[0-9]+$ ]]; then
    fail "Cargo version '${cargo_version}' is not semver X.Y.Z"
fi
cargo_major="${BASH_REMATCH[1]}"

if [[ "${cargo_major}" != "${laravel_major}" ]]; then
    fail "ext-rabbit_rs major ${laravel_major} is not compatible with Cargo major ${cargo_major} — refusing to split"
fi
ok "major versions compatible: ${cargo_major}"

# --- list files that would be copied ------------------------------------------

echo "==> Scanning Laravel package contents"
pkg_files=()
while IFS= read -r -d '' f; do
    rel="${f#"${LARAVEL_PKG_DIR}/"}"
    pkg_files+=("${rel}")
done < <(find "${LARAVEL_PKG_DIR}" -type f \
    -not -name '.gitkeep' \
    -not -path '*/vendor/*' \
    -not -path '*/node_modules/*' \
    -not -path '*/.git/*' \
    -not -path '*/.phpunit.cache/*' \
    -print0)

if [[ ${#pkg_files[@]} -eq 0 ]]; then
    fail "no files found in ${LARAVEL_PKG_DIR}"
fi
ok "${#pkg_files[@]} files found in Laravel package"

# --- dry-run mode -------------------------------------------------------------

if [[ "${DRY_RUN}" -eq 1 ]]; then
    echo ""
    echo "==> DRY RUN — no files will be created"
    echo ""
    if [[ -z "${OUTPUT_DIR}" ]]; then
        echo "Output directory: (default: ./rabbit-rs-laravel-split)"
        OUTPUT_DIR_DISPLAY="./rabbit-rs-laravel-split"
    else
        OUTPUT_DIR_DISPLAY="${OUTPUT_DIR}"
    fi
    echo ""
    echo "Would copy these files to ${OUTPUT_DIR_DISPLAY}/:"
    for f in "${pkg_files[@]}"; do
        echo "  ${f}"
    done
    echo ""
    echo "composer.json will be at: ${OUTPUT_DIR_DISPLAY}/composer.json"
    echo ""
    echo "Dry-run complete. ${#pkg_files[@]} files would be copied."
    exit 0
fi

# --- real run -----------------------------------------------------------------

if [[ -z "${OUTPUT_DIR}" ]]; then
    OUTPUT_DIR="${ROOT_DIR}/rabbit-rs-laravel-split"
fi

if [[ -e "${OUTPUT_DIR}" ]]; then
    fail "output directory already exists: ${OUTPUT_DIR}"
fi

echo "==> Creating output directory: ${OUTPUT_DIR}"
mkdir -p "${OUTPUT_DIR}"

echo "==> Copying files"
for f in "${pkg_files[@]}"; do
    src="${LARAVEL_PKG_DIR}/${f}"
    dst="${OUTPUT_DIR}/${f}"
    mkdir -p "$(dirname "${dst}")"
    cp "${src}" "${dst}"
    echo "  ${f}"
done

# Verify composer.json is at the root
if [[ ! -f "${OUTPUT_DIR}/composer.json" ]]; then
    fail "composer.json is not at the root of ${OUTPUT_DIR}"
fi

# Validate the copied composer.json
echo "==> Validating split composer.json"
split_name="$(jq -r '.name' "${OUTPUT_DIR}/composer.json")"
[[ "${split_name}" == "goopil/rabbit-rs-laravel" ]] \
    || fail "split composer.json name is '${split_name}', expected 'goopil/rabbit-rs-laravel'"

split_ext_req="$(jq -r '.require."ext-rabbit_rs"' "${OUTPUT_DIR}/composer.json")"
[[ "${split_ext_req}" == "${ext_req}" ]] \
    || fail "split ext-rabbit_rs constraint changed: '${split_ext_req}' != '${ext_req}'"
ok "split composer.json is valid"

echo ""
echo "Split complete: ${OUTPUT_DIR}"
echo "  ${#pkg_files[@]} files copied"
echo "  composer.json at root: yes"
