#!/usr/bin/env bash
# Shared helpers for building and loading the ext-rabbit_rs PHP extension in test scripts.
#
# Usage: source this file from other scripts in scripts/.
#
# Functions provided:
#   ext_project_root       — echo the project root directory
#   ext_artifact_path      — echo the path to the extension artifact (debug or release)
#   ext_build_debug        — build the extension in debug mode with --features extension-tests
#   ext_ensure_built       — build the extension if the artifact is missing
#   ext_verify_loads       — verify the extension loads correctly
#   ext_php_cmd            — echo a PHP command that loads the local extension (php -d extension=<artifact>)
#   ext_php_no_ext_cmd     — echo a plain PHP command (no extension loaded)
#
# The extension should NOT be installed system-wide on the development machine.
# If it is, remove it first: ./scripts/install.sh --remove  (or cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes)
# Test scripts load the extension from target/debug/ (or target/release/) via -d extension=<artifact>.

set -euo pipefail

# Resolve project root from the directory of the calling script.
ext_project_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[1]}")" && pwd)"
    cd "${script_dir}/.." && pwd
}

# Detect the extension artifact suffix based on the OS.
_ext_suffix() {
    case "$(uname -s)" in
        Darwin) echo "dylib" ;;
        Linux)  echo "so" ;;
        *)      echo "ERROR: unsupported operating system: $(uname -s)" >&2; exit 1 ;;
    esac
}

# Echo the path to the extension artifact.
# Prefers target/debug/ (matching the default cargo build profile used by test scripts).
# Falls back to target/release/ if debug is absent.
ext_artifact_path() {
    local root
    root="$(ext_project_root)"
    local suffix
    suffix="$(_ext_suffix)"
    local debug_artifact="${root}/target/debug/librabbit_rs_php.${suffix}"
    local release_artifact="${root}/target/release/librabbit_rs_php.${suffix}"

    if [[ -f "${debug_artifact}" ]]; then
        echo "${debug_artifact}"
    elif [[ -f "${release_artifact}" ]]; then
        echo "${release_artifact}"
    else
        # Neither exists — return the debug path (the canonical default).
        echo "${debug_artifact}"
    fi
}

# Build the extension in debug mode with test support enabled.
ext_build_debug() {
    local root
    root="$(ext_project_root)"
    echo "=== Building ext-rabbit_rs ==="
    cargo build \
        --manifest-path "${root}/crates/rabbit-rs-php/Cargo.toml" \
        --features extension-tests
}

# Ensure the extension artifact exists, building if necessary.
ext_ensure_built() {
    local artifact
    artifact="$(ext_artifact_path)"
    if [[ ! -f "${artifact}" ]]; then
        ext_build_debug
    fi
    if [[ ! -f "${artifact}" ]]; then
        echo "ERROR: extension artifact not found after build: ${artifact}" >&2
        exit 1
    fi
}

# Verify that the extension loads correctly.
ext_verify_loads() {
    local artifact
    artifact="$(ext_artifact_path)"
    local php_bin="${PHP_BIN:-php}"

    if ! command -v "${php_bin}" >/dev/null 2>&1; then
        echo "ERROR: php not found" >&2
        exit 1
    fi

    echo "=== Verifying ext-rabbit_rs loads ==="
    local modules
    modules="$("${php_bin}" -d "extension=${artifact}" -m 2>/dev/null || true)"
    if ! grep -q rabbit_rs <<< "${modules}"; then
        echo "ERROR: ext-rabbit_rs failed to load" >&2
        echo "  PHP SAPI: $(${php_bin} -r 'echo php_sapi_name();' 2>/dev/null)"
        echo "  PHP version: $(${php_bin} -r 'echo phpversion();' 2>/dev/null)"
        exit 1
    fi
    echo "ext-rabbit_rs is loaded."
}

# Echo a PHP command that loads the local extension from target/.
# Usage: PHP_CMD=( $(ext_php_cmd) ); "${PHP_CMD[@]}" vendor/bin/pest
ext_php_cmd() {
    local artifact
    artifact="$(ext_artifact_path)"
    local php_bin="${PHP_BIN:-php}"
    echo "${php_bin}" -d "extension=${artifact}"
}

# Echo a plain PHP command with no extension loaded.
# Used for Unit/Feature tests that must run without ext-rabbit_rs.
ext_php_no_ext_cmd() {
    local php_bin="${PHP_BIN:-php}"
    echo "${php_bin}"
}
