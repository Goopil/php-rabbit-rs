# Contributing to Rabbit RS

Rabbit RS is a monorepo with three components: a Rust core, a native PHP extension, and a Laravel queue driver. This guide gets you from clone to running tests in under 5 minutes.

## Prerequisites

- **Rust** 1.96.0 (pinned in `rust-toolchain.toml`)
- **PHP** 8.4 or 8.5 with development headers
- **Composer** for PHP dependencies
- **Docker** for the RabbitMQ test lab (integration tests only)
- **cargo-php** for installing the extension locally: `cargo install cargo-php`

## Quick start

```bash
# Clone
git clone https://github.com/Goopil/rabbit-rs.git
cd rabbit-rs

# Build the extension (debug mode)
cargo build -p rabbit-rs-php --features extension-tests

# Run Laravel tests (no extension needed for Unit/Feature)
./scripts/test-laravel.sh

# Run PHP extension tests (Pest + PHPT)
./scripts/test-extension.sh

# Run the full quality gate
./scripts/check.sh
```

## Project structure

| Directory | Language | Role |
|-----------|----------|------|
| `crates/rabbit-rs-core/` | Rust | Runtime-independent core: pooling, topology, publishing, consuming, recovery |
| `crates/rabbit-rs-php/` | Rust → C ABI | `cdylib` that exposes the core to PHP via `ext-php-rs` |
| `packages/laravel-queue/` | PHP | Laravel queue driver on top of the native extension |
| `benchmarks/` | PHP | Benchmark suite (4 drivers, 3 scenarios) |

Dependency flow: `rabbit-rs-core` → `rabbit-rs-php` (compiles to `.so`/`.dylib`) → loaded by PHP → consumed by `laravel-queue`.

## Running tests

| What | Command | Needs |
|------|---------|-------|
| Rust unit tests | `cargo test -p rabbit-rs-core` | Rust only |
| PHP extension tests | `./scripts/test-extension.sh` | Extension built |
| Laravel Unit/Feature | `./scripts/test-laravel.sh` | No extension |
| Laravel Integration | `./scripts/test-laravel.sh tests/Integration` | Extension + RabbitMQ lab |
| Octane lifecycle | `./scripts/test-octane.sh` | No extension |
| FPM isolation | `./scripts/test-fpm.sh` | Extension (release build) |
| Full integration | `./scripts/test-integration.sh` | Extension + Docker |
| Quality gate | `./scripts/check.sh` | Rust + Composer |

## Where to go next

- **Architecture and build details**: [docs/development.md](docs/development.md)
- **Coding conventions and invariants**: [AGENTS.md](AGENTS.md)
- **Troubleshooting runtime issues**: [docs/troubleshooting.md](docs/troubleshooting.md)
- **Configuration reference**: [docs/configuration.md](docs/configuration.md)

## Before opening a PR

1. Run `./scripts/check.sh` — must pass cleanly
2. Run `cargo fmt --all` after any Rust edits
3. Keep commits logical and scoped — don't include build artifacts, `.air/`, or IDE metadata
4. If you changed behavior, update the relevant doc in `docs/`
