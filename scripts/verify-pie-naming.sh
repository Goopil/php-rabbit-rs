#!/usr/bin/env bash
set -euo pipefail

# verify-pie-naming.sh — convention test for PIE release asset naming.
#
# Verifies that every asset of the release matrix (release/pie-matrix.json)
# matches the naming pattern expected by PIE 1.5.x for pre-packaged binaries
# (php/pie, src/Platform/PrePackagedBinaryAssetName.php), and that the naming
# is unified across:
#   - the packaging script (scripts/package-pie-binary.sh)
#   - the release workflow (.github/workflows/release.yml)
#   - the documented convention (docs/distribution.md)
#
# PIE accepts NTS asset names with or without an explicit -nts suffix, but
# requires the -zts suffix for ZTS builds. The Rabbit RS convention is that
# every Linux artifact carries an explicit thread-safety suffix (-nts / -zts)
# so that asset names are unambiguous and self-describing.
#
# Usage:
#   ./scripts/verify-pie-naming.sh

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIE_MATRIX="${ROOT_DIR}/release/pie-matrix.json"
WORKFLOW="${ROOT_DIR}/.github/workflows/release.yml"
DOCS="${ROOT_DIR}/docs/distribution.md"
PKG_SCRIPT="${ROOT_DIR}/scripts/package-pie-binary.sh"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

need_file() {
    [[ -f "$1" ]] || fail "required file missing: $1"
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

need_file "${PIE_MATRIX}"
need_file "${WORKFLOW}"
need_file "${DOCS}"
need_file "${PKG_SCRIPT}"
need_cmd jq

# --- packaging script naming logic --------------------------------------------

echo "==> Checking packaging script naming logic"
"${PKG_SCRIPT}" --self-test

# --- matrix: size, suffix invariant, PIE candidate membership ------------------

echo "==> Checking PIE matrix naming"

matrix_count="$(jq -r '.matrix | length' "${PIE_MATRIX}")"
[[ "${matrix_count}" -eq 16 ]] \
    || fail "PIE matrix must have exactly 16 entries, found ${matrix_count}"

php_ext_name="$(jq -r '.php_extension_name' "${PIE_MATRIX}")"
[[ "${php_ext_name}" == "rabbit_rs" ]] \
    || fail "php_extension_name is '${php_ext_name}', expected 'rabbit_rs'"

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

declare -a names
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

    names+=("${archive}")
done
ok "all ${matrix_count} asset names carry an explicit thread-safety suffix and match the PIE naming pattern"

# --- uniqueness ----------------------------------------------------------------

unique_count="$(printf '%s\n' "${names[@]}" | sort -u | wc -l | tr -d ' ')"
[[ "${unique_count}" -eq "${matrix_count}" ]] \
    || fail "expected ${matrix_count} unique asset names, found ${unique_count}"
ok "all ${matrix_count} asset names are unique"

# --- workflow consistency ------------------------------------------------------

echo "==> Checking release workflow naming"

# The Linux build job must append the explicit thread-safety suffix from the
# matrix variable, never an implicit empty suffix for NTS.
grep -q -- 'ASSET_BASE="php_rabbit_rs-v${VERSION}_php${{ matrix.php }}-${{ matrix.arch }}-linux-${{ matrix.libc }}-${{ matrix.ts }}"' "${WORKFLOW}" \
    || fail "release.yml build-linux Package step does not produce the unified naming pattern (explicit -\${{ matrix.ts }} suffix)"

# macOS artifacts are outside the PIE matrix (os-families: linux) and are
# consumed by Homebrew; they keep the explicit -nts suffix.
grep -q 'arm64-darwin-nts' "${WORKFLOW}" \
    || fail "release.yml macOS Package step lost the explicit -nts suffix"
ok "release workflow produces the unified naming pattern"

# --- documentation consistency -------------------------------------------------

echo "==> Checking documented convention"
grep -q 'php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip' "${DOCS}" \
    || fail "docs/distribution.md does not document the unified naming pattern"
ok "docs/distribution.md documents the unified naming pattern"

echo ""
echo "All PIE naming convention checks passed."
