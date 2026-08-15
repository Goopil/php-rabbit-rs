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
# Note: cargo php stubs loads the extension via the PHP embed SAPI.
# If the PHP build lacks embed (e.g. Homebrew default), this will abort.
# The authoritative stub is crates/rabbit-rs-php/stubs/rabbit_rs.stub.php,
# validated by php -l and PHPT reflection tests.
exec cargo php stubs --manifest "${MANIFEST}" "$@"
