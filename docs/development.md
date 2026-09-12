# Development Guide

This guide covers the architecture, build system, test strategy, and common pitfalls for developers working on Rabbit RS. For a quick start, see [CONTRIBUTING.md](../CONTRIBUTING.md). For coding conventions, see [AGENTS.md](../AGENTS.md).

## Architecture overview

Rabbit RS is a monorepo with three layers, each in a different language:

```
┌─────────────────────────────────────────────────┐
│  packages/laravel-queue/                        │
│  Laravel queue driver (PHP)                     │
│  - RabbitMqConnector, RabbitMqQueue, etc.       │
│  - ConnectionCompiler maps queue config → native│
└────────────────────┬────────────────────────────┘
                     │ calls
┌────────────────────▼────────────────────────────┐
│  crates/rabbit-rs-php/                          │
│  Native PHP extension (Rust → C ABI → PHP)      │
│  - Pool, Consumer, Delivery classes             │
│  - ext-php-rs bindings                           │
└────────────────────┬────────────────────────────┘
                     │ depends on
┌────────────────────▼────────────────────────────┐
│  crates/rabbit-rs-core/                          │
│  Runtime-independent core (Rust)                │
│  - Connection pooling, topology, publishing     │
│  - Consuming, recovery, metrics                 │
│  - Transport abstraction (Lapin behind it)      │
└─────────────────────────────────────────────────┘
```

The core knows nothing about PHP. The PHP extension layer translates between PHP types (Zend values) and Rust types. The Laravel layer translates between Laravel abstractions (jobs, queues, workers) and the native extension API.

## Workspace layout

### `crates/rabbit-rs-core/` — Rust core

The runtime-independent heart of the project. All AMQP logic lives here, behind a `Transport` trait so broker behavior is mockable.

| Path | Role |
|------|------|
| `src/config.rs` | Configuration parsing and validation |
| `src/pool.rs` | Connection pooling, per-vhost connections |
| `src/publisher.rs` | Publisher confirms, mandatory returns, replay buffer |
| `src/consumer.rs` | Consumer channels, delivery buffering |
| `src/recovery.rs` | Connection recovery (deterministic order) |
| `src/topology.rs` | Exchange, queue, binding declarations |
| `src/metrics.rs` | Pool and consumer metrics |
| `src/transport/` | Transport abstraction (Lapin implementation + mock) |
| `tests/` | Integration tests (13 files: blind_pump, consumer, integration, log_facade, metrics, poison, pool_clear, publisher, recovery, tls_integration, topology, transport_liveness, transport_tuning) |

**Key commands:**
```bash
cargo test -p rabbit-rs-core                              # all tests
cargo test -p rabbit-rs-core config::tests                # focused
cargo test -p rabbit-rs-core --test publisher_safety      # specific test file
cargo test -p rabbit-rs-core --features integration       # with RabbitMQ lab
```

### `crates/rabbit-rs-php/` — PHP extension

Compiles to a `cdylib` (`librabbit_rs_php.so` on Linux, `.dylib` on macOS). Uses `ext-php-rs` to expose Rust classes to PHP.

| Path | Role |
|------|------|
| `src/lib.rs` | Module entry point (`get_module`) |
| `src/classes/` | PHP classes: Pool, Consumer, Delivery, Exception |
| `src/conversion.rs` | PHP ↔ Rust type conversion |
| `src/callbacks.rs` | PHP callback invocation (connection state, backpressure) |
| `src/testing.rs` | Test helpers (behind `extension-tests` feature) |
| `stubs/rabbit_rs.stub.php` | Generated PHP stub (`cargo php stubs`; docblocks live in the Rust `///` docs) |
| `tests/` | Pest tests (Extension, Pool, Publisher, Consumer, Config, etc.) |
| `tests/phpt/` | PHPT tests (run via `run-tests.php`) |
| `tests/fixtures/fpm/` | FPM config for the isolation test |

**Key commands:**
```bash
cargo build -p rabbit-rs-php --features extension-tests   # debug build
./scripts/test-extension.sh                               # Pest + PHPT
./scripts/install.sh --release                            # install into PHP
```

### `packages/laravel-queue/` — Laravel queue driver

Pure PHP package (`goopil/rabbit-rs-laravel`). Uses Pest for tests.

| Path | Role |
|------|------|
| `src/Connectors/` | `RabbitMqConnector` — queue connector |
| `src/Queue/` | `RabbitMqQueue` — push, pop, later, bulk, size, clear |
| `src/Jobs/` | `RabbitMqJob` — job wrapper around native Delivery |
| `src/Console/` | `rabbit-rs:work`, `rabbit-rs:status`, and `rabbit-rs:doctor` commands |
| `src/Octane/` | Octane lifecycle hooks (flush, reload, stop) |
| `src/Config/` | `ConnectionCompiler` — compiles queue.php connections to native config |
| `src/Support/` | `NativePoolFactory` — pool factory with fork safety |
| `tests/Unit/` | Unit tests (fake classes, no extension) |
| `tests/Feature/` | Feature tests (fake classes, no extension) |
| `tests/Integration/` | Integration tests (real extension + RabbitMQ) |

**Key commands:**
```bash
./scripts/test-laravel.sh                                  # Unit + Feature
./scripts/test-laravel.sh tests/Integration               # Integration (needs lab)
./scripts/test-octane.sh                                   # Octane lifecycle
```

### `benchmarks/` — Benchmark suite

PHP benchmark suite comparing `rabbit-rs` (native), `php-amqplib` (pure PHP), and `amqp-ext` (C bindings).

| Path | Role |
|------|------|
| `src/AbstractBenchmark.php` | Base class with timing and recording |
| `src/Drivers/` | Driver implementations (RabbitRs, AmqpLib, AmqpExt, Laravel) |
| `src/Config.php` | Connection config from environment |
| `src/run-benchmarks.php` | Entry point |

## Build system

### Building the extension

```bash
# Debug build (for development)
cargo build -p rabbit-rs-php --features extension-tests

# Release build (for installation)
cargo build --release -p rabbit-rs-php

# Install into the current PHP
./scripts/install.sh --release
```

The `--features extension-tests` flag enables test helpers in the extension (registered in `src/testing.rs`). Without it, tests that call internal functions will fail.

**Output artifacts:**
- `target/debug/librabbit_rs_php.{dylib|so}` — debug build
- `target/release/librabbit_rs_php.{dylib|so}` — release build

### Why `cargo-php` needs wrapper scripts

`cargo php install` and `cargo php stubs` fail at the workspace root because the root `Cargo.toml` is a workspace manifest, not a package manifest. `cargo-php` (v0.1.21) does not resolve workspace members automatically.

The wrapper scripts pass `--manifest crates/rabbit-rs-php/Cargo.toml` under the hood:

- `./scripts/install.sh` → wraps `cargo php install`
- `./scripts/stubs.sh` → wraps `cargo php stubs`

### Stub generation

Since cargo-php 0.1.21, `cargo php stubs` does not require the PHP embed SAPI: it builds the extension, dlopens the cdylib, and reads the exported `ext_php_rs_describe_module` metadata. Use `./scripts/stubs.sh`, which passes `--manifest crates/rabbit-rs-php/Cargo.toml`.

On macOS, cargo-php 0.1.21 fails to link its own binary (its build script only carries the Linux link flag), so install it with:

```bash
RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php
```

The authoritative stub is `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, generated by `cargo php stubs` and validated by `php -l`, the Pest suite, and the PHPT tests. The docblocks rendered in the stub live in the Rust `///` doc comments of `src/classes/*.rs` — edit there, then regenerate.

## Test strategy

### Rust core tests

- **Unit tests** live next to their modules (`#[cfg(test)]` blocks).
- **Integration tests** live in `crates/rabbit-rs-core/tests/` (13 files).
- Tests use **paused Tokio time** and a **scriptable mock transport** — no real sleeps, no real broker.
- Some integration tests require a live RabbitMQ lab (behind the `integration` feature flag).

### PHP extension tests

Two types:

| Type | Runner | What it tests |
|------|--------|---------------|
| Pest | `vendor/bin/pest` | PHP-level behavior (config validation, pool registry, publisher outcomes, consumer state, secrets, reflection) |
| PHPT | `run-tests.php` | Native extension metadata, reflection against the stub |

`./scripts/test-extension.sh` runs both. It:
1. Resolves `php` and `php-config` from `PATH`
2. Finds `run-tests.php` in the PHP build directory
3. Builds the extension with `--features extension-tests`
4. Runs Pest tests
5. Runs PHPT tests

### Laravel tests

Three tiers:

| Tier | Extension needed | RabbitMQ needed | What it tests |
|------|-----------------|-----------------|---------------|
| Unit | No | No | Config normalization, job lifecycle, queue operations (with fake classes) |
| Feature | No | No | Multi-vhost worker, Octane lifecycle, status command, work command (with fake classes) |
| Integration | Yes | Yes | Real publish/consume against RabbitMQ lab |

Unit and Feature tests use **fake classes** defined in `tests/bootstrap.php` and `tests/Pest.php` that simulate the extension's classes. This is why they must run **without** the extension loaded — the "missing extension" assertion in `RabbitMqServiceProviderTest` would fail if the real extension were present.

### Test scripts and extension loading

The extension should **not** be installed system-wide on the development machine. Test scripts load it from `target/debug/` (or `target/release/`) via `-d extension=<artifact>` when needed.

- `php` = run without the extension (Unit/Feature tests)
- `php -d extension=<artifact>` = load the local build (Integration, extension tests)
- PHPT tests use `php -n` (standard `run-tests.php` isolation, not related to our loading strategy)

If the extension is installed system-wide (e.g. via `./scripts/install.sh`), remove it first:

```bash
cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes
# Also remove any ext-rabbit_rs.ini in the PHP conf.d directory
rm -f $(php --ini | grep conf.d | head -1 | awk '{print $1}')/ext-rabbit_rs.ini
```

The shared helpers in `scripts/lib-extension.sh` encapsulate the loading:

| Function | What it does |
|----------|-------------|
| `ext_artifact_path()` | Resolves `target/debug/` or `target/release/` artifact |
| `ext_ensure_built()` | Builds with `--features extension-tests` if missing |
| `ext_verify_loads()` | Verifies the extension loads via `php -d extension= -m` |
| `ext_php_cmd()` | Echoes `php -d extension=<artifact>` |
| `ext_php_no_ext_cmd()` | Echoes `php` (no extension at all) |

### "I want to test X" reference

| I want to... | Command |
|-------------|---------|
| Test a Rust module | `cargo test -p rabbit-rs-core <module>::tests` |
| Test the PHP extension | `./scripts/test-extension.sh` |
| Test Laravel without the extension | `./scripts/test-laravel.sh` |
| Test Laravel with the extension | `./scripts/test-laravel.sh --with-extension` |
| Test Laravel integration | `./scripts/test-laravel.sh tests/Integration` |
| Test Octane lifecycle | `./scripts/test-octane.sh` |
| Test FPM isolation | `./scripts/test-fpm.sh` |
| Run the full quality gate | `./scripts/check.sh` |
| Run benchmarks | See `benchmarks/README.md` |

## RabbitMQ lab

Integration tests need a live RabbitMQ cluster. The lab is a 3-node Docker Compose setup.

```bash
# Start the lab (with delayed message exchange plugin)
./scripts/lab-up.sh with-plugin

# Wait until ready (checks cluster, vhosts, Prometheus)
./scripts/lab-ready.sh

# Stop the lab
./scripts/lab-down.sh
```

**What the lab provides:**
- 3 RabbitMQ nodes (clustered)
- 2 vhosts: `/orders-eu`, `/billing`
- Management UI at `http://localhost:15672` (admin / admin_lab)
- Prometheus at `http://localhost:9091`
- AMQP on ports 5672, 5673, 5675

**Profiles:**
- `with-plugin` — includes `rabbitmq_delayed_message_exchange` plugin
- `without-plugin` — no delayed message plugin, used for fallback (TTL) testing

`./scripts/test-integration.sh` handles the full cycle: start lab, wait for readiness, run Rust integration tests, build extension, run Laravel integration tests, stop lab.

## Extension loading patterns

### Pattern 1: Tests that need the extension

```bash
# Build the extension
cargo build -p rabbit-rs-php --features extension-tests

# Run tests with the local extension loaded from target/
php -d extension=target/debug/librabbit_rs_php.dylib vendor/bin/pest
```

The extension is loaded from `target/` via `-d extension=`. It should not be installed system-wide on the dev machine.

### Pattern 2: Tests that must run without the extension

```bash
# Run with plain php (no extension loaded)
php vendor/bin/pest
```

This is required for Laravel Unit/Feature tests because:
1. They use fake classes that simulate the extension's API
2. `RabbitMqServiceProviderTest` asserts that the provider throws when the extension is missing
3. Loading the real extension would conflict with the fake classes

### Pattern 3: System-wide installation (not recommended for dev)

```bash
# Install the extension into the current PHP
./scripts/install.sh --release

# Now any PHP invocation loads it automatically
php -m | grep rabbit_rs
```

If the extension is installed system-wide, test scripts will emit "Module already loaded" warnings when they also load it via `-d extension=`. Remove it before running tests:

```bash
cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes
```

## Common pitfalls

### `cargo php install` fails at the workspace root

**Cause:** `cargo-php` cannot resolve workspace members.

**Fix:** Use `./scripts/install.sh` which passes `--manifest crates/rabbit-rs-php/Cargo.toml`.

### `cargo php stubs` aborts with exit 134 on macOS

**Cause:** cargo-php ≤ 0.1.11 compiled a stubs binary linked against the PHP embed SAPI, which Homebrew PHP does not include.

**Fix:** Use cargo-php ≥ 0.1.21 (dlopen-based, no embed needed): `RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php`, then `./scripts/stubs.sh`.

### Tests fail with "Module already loaded" warnings

**Cause:** The extension is installed system-wide and the test script also loads it via `-d extension=`.

**Fix:** Remove the system-wide installation: `cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes`. Also delete any `ext-rabbit_rs.ini` in the PHP conf.d directory. Test scripts load the extension from `target/` only.

### Laravel Unit tests fail when the extension is loaded

**Cause:** Unit/Feature tests use fake classes that conflict with the real extension classes.

**Fix:** Run without the extension: `./scripts/test-laravel.sh` (no `--with-extension` flag).

### `test-fpm.sh` says "extension artifact not found"

**Cause:** FPM tests need a release build, not debug. Or you haven't built at all.

**Fix:** `cargo build --release -p rabbit-rs-php --features extension-tests` or use `ext_ensure_built` from `lib-extension.sh` which checks both `target/debug/` and `target/release/`.

### Rust tests hang or fail with connection errors

**Cause:** The test requires the RabbitMQ lab but it's not running.

**Fix:** Start the lab: `./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh`. Or run only the unit tests: `cargo test -p rabbit-rs-core` (without `--features integration`).

## Release candidate (RC) pipeline

`scripts/verify-release-candidate.sh` runs the full release-candidate tier
list — the same tiers `.github/workflows/release-candidate.yml` runs on
`v*-*rc*` tags, with the tier output teed into an evidence directory
(`target/rc-evidence/<timestamp>/`, uploaded as workflow artifacts in CI).

```bash
./scripts/verify-release-candidate.sh --dry-run     # prerequisites + tier plan, no tiers
./scripts/verify-release-candidate.sh               # full RC pass (~2-3 h)
```

### Prerequisites

Run on a quiet machine: the orchestrator manages the RabbitMQ lab itself
(started before tier 5, shared by tiers 5, 6, 8, 9, stopped at the end).

- Docker (daemon running) and the usual shell tooling (`jq`, `curl`, `git`)
- Rust 1.96 (pinned by `rust-toolchain.toml`)
- PHP 8.4+ with `php-config` (and `php-fpm` for tier 5)
- `/etc/hosts` mapping for the SAN-negative TLS case —
  `sudo sh -c 'echo "127.0.0.1 wrong.internal" >> /etc/hosts'` (CI adds the
  same line in the integration job)
- Composer vendors: `packages/laravel-queue`, `crates/rabbit-rs-php`,
  `benchmarks/driver-bench` (auto-installed by the tiers when missing)
- Built extension artifact (`target/debug/librabbit_rs_php.*`, auto-built by
  the tiers; `target/release/librabbit_rs_php.dylib` for tier 9 — built on
  demand on macOS)

`--dry-run` validates all of this and exits non-zero listing what is missing.

### Expected duration

| Tier | Script | Estimate |
|------|--------|----------|
| 1 | `scripts/check.sh` (fmt, clippy, full test suite, composer, cargo-deny) | ~10-25 min |
| 2 | `scripts/test-extension.sh` (Pest + PHPT) | ~5-10 min |
| 3 | `scripts/test-laravel.sh` (Unit + Feature) | ~2-5 min |
| 4 | `scripts/test-integration.sh --with-tls` (Rust + Laravel integration on the TLS lab) | ~10-20 min |
| 5 | `scripts/test-fpm.sh` (broker-backed FPM certification) | ~3-5 min |
| 6 | `scripts/test-octane-runtime.sh` × 4 servers | ~40-80 min |
| 7 | `scripts/validate-distribution.sh` | ~1-2 min |
| 8 | `scripts/amqp-smoke.sh` per matching artifact in `release/` | ~2-5 min each |
| 9 | Fresh rebench + `check-budgets.php` self-baseline | ~10-20 min |
| — | **Total** | **~2-3 h** |

### Blocking vs advisory

| Tier | Verdict | Blocking? | Notes |
|------|---------|-----------|-------|
| 1-4 | PASS/FAIL | yes | Fail-fast: a failure SKIPs the remaining tiers with an explicit note |
| 5 | PASS/FAIL | yes | Runs in external-lab mode against the shared lab |
| 6 | PASS/FAIL/SKIP | yes | Harness exit 2 ("server not available on this machine") is an explicit SKIP — its evidence comes from the nightly matrix, which provisions each server in its own job |
| 7 | PASS/FAIL | yes | Full artifact checks only when `release/` contains archives |
| 8 | PASS/FAIL/SKIP | yes | SKIP with a note when `release/` is empty or no artifact matches this platform (php version + arch + libc); other cells are covered by the functional matrix |
| 9 | PASS/FAIL/SKIP | losses/duplicates/ok: yes — ratio thresholds: advisory | Self-comparison by design: the committed baseline is single-machine, so the RC run re-baselines itself from its own fresh rebench output; only the integrity verdict (losses, duplicates, `ok`) blocks. SKIPs on Linux (rebench hardcodes the macOS `.dylib` path) — disclose the SKIP in the RC evidence |

A blocking failure exits non-zero after printing the final per-tier summary.
The go/no-go gate that consumes this evidence is
[docs/release-checklist.md](release-checklist.md).

## Coding conventions

See [AGENTS.md](../AGENTS.md) for the full list. Key points:

- **Unsafe Rust is forbidden.** Do not weaken `#![forbid(unsafe_code)]`.
- **Lapin stays behind the Transport abstraction.** Broker behavior must remain mockable.
- **PHP tests use Pest**, not PHPUnit.
- **No real sleeps in unit tests.** Use paused Tokio time and the mock transport.
- **Errors are typed** with actionable context, not strings.
- **All queues, channels, and buffers are explicitly bounded.**
- **Never expose credentials** through Debug, errors, metrics, or logs.

## Before opening a PR

1. `./scripts/check.sh` passes cleanly (fmt + clippy + test + cargo deny + composer validate)
2. `cargo fmt --all` applied after Rust edits
3. Commits are logical and scoped — no build artifacts, `.air/`, or IDE metadata
4. If you changed behavior, update the relevant doc in `docs/`
5. If you completed a planned task, update the implementation plan in `docs/plans/`
