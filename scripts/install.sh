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
# Pass --release by default for a production build; override with flags as needed.
exec cargo php install --manifest "${MANIFEST}" "$@"
