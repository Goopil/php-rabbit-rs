#!/usr/bin/env bash
# Shared helpers for building and loading the ext-rabbit_rs PHP extension in test scripts.
#
# Usage: source this file from other scripts in scripts/.
#
# Functions provided:
#   ext_project_root       — echo the project root directory
#   ext_artifact_path      — echo the path to the extension artifact (debug preferred,
#                            builds it when missing OR stale, warns loudly before any
#                            stale release fallback)
#   ext_build_debug        — build the extension in debug mode with --features extension-tests
#   ext_ensure_built       — build the extension if the artifact is missing or stale
#   ext_verify_loads       — verify the extension loads correctly
#   ext_php_cmd            — echo a PHP command that loads the local extension (php -d extension=<artifact>)
#   ext_php_no_ext_cmd     — echo a plain PHP command (no extension loaded)
#
# Rebuild-on-change: the debug artifact is rebuilt when it is older than the
# crate sources/manifests, or when something else rebuilt it after the last
# feature build (a bare `cargo build`/`cargo test` of the workspace drops
# --features extension-tests, which silently breaks test suites). A feature
# stamp file under target/debug/ records the last feature build.
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
# Prefers target/debug/ (matching the default cargo build profile used by test
# scripts). When the debug artifact is absent, builds it instead of silently
# reusing a stale release binary (T1 Phase C: 39 bogus Pest failures caused by
# a stale release artifact without the extension-tests feature). When the
# debug artifact exists but is STALE (sources newer than the artifact, or the
# artifact was rebuilt without --features extension-tests by another cargo
# command), it is rebuilt before being returned. If we must fall back to
# target/release/ anyway, a loud warning comparing the artifact mtime with the
# current HEAD commit date is printed so a stale binary can never fail tests
# silently.
ext_artifact_path() {
    local root
    root="$(ext_project_root)"
    local suffix
    suffix="$(_ext_suffix)"
    local debug_artifact="${root}/target/debug/librabbit_rs_php.${suffix}"
    local release_artifact="${root}/target/release/librabbit_rs_php.${suffix}"

    if [[ -f "${debug_artifact}" ]] && _ext_artifact_is_stale "${debug_artifact}"; then
        _ext_warn_stale_debug "${debug_artifact}"
        # Its output goes to stderr so command substitution stays clean.
        ext_build_debug >&2 || true
    fi

    if [[ -f "${debug_artifact}" ]]; then
        echo "${debug_artifact}"
        return 0
    fi

    # Footgun guard: never silently reuse a stale release binary. Build the
    # debug artifact first; its output goes to stderr so command substitution
    # stays clean.
    ext_build_debug >&2 || true

    if [[ -f "${debug_artifact}" ]]; then
        echo "${debug_artifact}"
        return 0
    fi

    if [[ -f "${release_artifact}" ]]; then
        _ext_warn_stale_release "${root}" "${release_artifact}"
        echo "${release_artifact}"
        return 0
    fi

    # Neither exists — return the debug path (the canonical default);
    # callers such as ext_ensure_built turn this into a hard error.
    echo "${debug_artifact}"
}

# Echo the path of the feature stamp recording the last
# --features extension-tests build.
_ext_feature_stamp() {
    local root
    root="$(ext_project_root)"
    echo "${root}/target/debug/.rabbit-rs-ext-feature-stamp"
}

# Return 0 when at least one source, manifest, or lockfile that feeds the
# extension artifact is newer than the artifact itself.
_ext_sources_newer_than() {
    local artifact="$1"
    local root
    root="$(ext_project_root)"
    local newer
    newer="$(find \
        "${root}/crates/rabbit-rs-php/src" \
        "${root}/crates/rabbit-rs-php/Cargo.toml" \
        "${root}/crates/rabbit-rs-php/build.rs" \
        "${root}/crates/rabbit-rs-core/src" \
        "${root}/crates/rabbit-rs-core/Cargo.toml" \
        "${root}/Cargo.toml" \
        "${root}/Cargo.lock" \
        -type f -newer "${artifact}" -print -quit 2>/dev/null || true)"
    [[ -n "${newer}" ]]
}

# Return 0 when the debug artifact must be rebuilt:
# 1. a source, manifest, or lockfile is newer than the artifact, or
# 2. the feature stamp is missing (never feature-built), or
# 3. the artifact is newer than the stamp — something else rebuilt it
#    (e.g. check.sh's workspace test build) without --features extension-tests.
_ext_artifact_is_stale() {
    local artifact="$1"
    local stamp
    stamp="$(_ext_feature_stamp)"

    if _ext_sources_newer_than "${artifact}"; then
        return 0
    fi

    if [[ ! -f "${stamp}" ]]; then
        return 0
    fi

    local artifact_mtime stamp_mtime
    artifact_mtime="$(_ext_file_mtime "${artifact}")"
    stamp_mtime="$(_ext_file_mtime "${stamp}")"
    if (( artifact_mtime > stamp_mtime )); then
        return 0
    fi

    return 1
}

# Warn loudly (stderr) that the debug artifact is stale and is being rebuilt,
# explaining which staleness signal fired.
_ext_warn_stale_debug() {
    local artifact="$1"
    local stamp
    stamp="$(_ext_feature_stamp)"
    local reason
    if _ext_sources_newer_than "${artifact}"; then
        reason="crate sources or manifests changed since the last build"
    elif [[ ! -f "${stamp}" ]]; then
        reason="no feature stamp found (the artifact was never built with --features extension-tests by these helpers)"
    else
        reason="the artifact was rebuilt by another cargo command after the last feature build (it likely lacks --features extension-tests)"
    fi

    {
        echo "WARNING: stale ext-rabbit_rs debug artifact detected (${reason}):"
        echo "  ${artifact}"
        echo "  Rebuilding it with --features extension-tests..."
    } >&2
}

# Print the modification time of a file as epoch seconds (GNU userland first,
# then BSD/macOS; echoes 0 when it cannot be determined). The GNU test must
# come first: `stat -f` on GNU means "filesystem status", which would succeed
# with unrelated output instead of falling through.
_ext_file_mtime() {
    stat -c '%Y' "$1" 2>/dev/null || stat -f '%m' "$1" 2>/dev/null || echo 0
}

# Format an epoch timestamp as UTC ISO-8601 (BSD date first, then GNU date,
# then the raw epoch as a last resort).
_ext_epoch_to_iso() {
    date -u -r "$1" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || date -u -d "@$1" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
        || echo "$1"
}

# Warn loudly (stderr) about falling back to the release artifact, comparing
# the artifact mtime with the current HEAD commit date so a stale binary
# cannot hide behind a passing-looking resolution.
_ext_warn_stale_release() {
    local root="$1"
    local artifact="$2"
    local mtime head_epoch age
    mtime="$(_ext_file_mtime "${artifact}")"
    head_epoch="$(git -C "${root}" log -1 --format=%ct 2>/dev/null || echo 0)"

    {
        echo "WARNING: target/debug extension artifact is missing; falling back to the RELEASE binary:"
        echo "  ${artifact}"
        if [[ "${mtime}" != 0 ]]; then
            echo "  artifact mtime: $(_ext_epoch_to_iso "${mtime}")"
        else
            echo "  artifact mtime: unknown"
        fi
        if [[ "${head_epoch}" != 0 && "${mtime}" != 0 ]]; then
            echo "  HEAD commit:    $(_ext_epoch_to_iso "${head_epoch}")"
            if (( mtime < head_epoch )); then
                age=$(( head_epoch - mtime ))
                echo "  This artifact PREDATES the current HEAD by ${age}s and is very likely stale."
            fi
        fi
        echo "  A stale release binary lacks --features extension-tests and makes test suites"
        printf '  fail in confusing ways (e.g. missing Goopil\\RabbitRs\\testing_pool).\n'
        echo "  Fix: cargo build --manifest-path crates/rabbit-rs-php/Cargo.toml --features extension-tests"
    } >&2
}

# Build the extension in debug mode with test support enabled, then record
# the feature stamp used by the staleness check.
ext_build_debug() {
    local root
    root="$(ext_project_root)"
    echo "=== Building ext-rabbit_rs ==="
    cargo build \
        --manifest-path "${root}/crates/rabbit-rs-php/Cargo.toml" \
        --features extension-tests || return 1

    local stamp
    stamp="$(_ext_feature_stamp)"
    mkdir -p "$(dirname "${stamp}")"
    touch "${stamp}"
}

# Ensure the extension artifact exists and is fresh, building when necessary.
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
