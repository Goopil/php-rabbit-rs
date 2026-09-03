#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
if command -v cargo-nextest >/dev/null 2>&1; then
    cargo nextest run --workspace --all-targets --no-fail-fast
else
    echo "::warning::cargo-nextest not found, falling back to cargo test" >&2
    cargo test --workspace --all-targets
fi
composer validate --strict

if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check
else
    echo "::warning::cargo-deny not found, skipping supply-chain check" >&2
fi
