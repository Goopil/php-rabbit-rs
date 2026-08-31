#!/usr/bin/env bash
# Shared helpers for release validation and packaging scripts.

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

sha256_func() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}
