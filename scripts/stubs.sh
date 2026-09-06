#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${ROOT_DIR}/crates/rabbit-rs-php/Cargo.toml"

if [[ ! -f "${MANIFEST}" ]]; then
    echo "Cargo manifest not found: ${MANIFEST}" >&2
    exit 1
fi

# cargo-php requires a package manifest, not a workspace manifest.
# The root Cargo.toml is workspace-only, so we point at the extension crate.
# cargo-php >= 0.1.21 dlopens the built cdylib to read its metadata: no PHP
# embed SAPI is needed. On macOS, install cargo-php with:
#   RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php
# The generated stub is crates/rabbit-rs-php/stubs/rabbit_rs.stub.php;
# its docblocks are maintained in the Rust /// docs of src/classes/*.rs.
exec cargo php stubs --manifest "${MANIFEST}" "$@"
