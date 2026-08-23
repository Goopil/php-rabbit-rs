# Development Guide

This guide covers the architecture, build system, test strategy, and common pitfalls for developers working on Rabbit RS. For a quick start, see [CONTRIBUTING.md](../CONTRIBUTING.md). For coding conventions, see [AGENTS.md](../AGENTS.md).

## Architecture overview

Rabbit RS is a monorepo with three layers, each in a different language:

```
┌─────────────────────────────────────────────────┐
│  packages/laravel-queue/                        │
│  Laravel queue driver (PHP)                     │
│  - RabbitMqConnector, RabbitMqQueue, etc.       │
│  - ConfigNormalizer maps Laravel config → native│
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
| `tests/` | Integration tests (6 files: publisher, consumer, recovery, topology, metrics, integration) |

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
| `stubs/rabbit_rs.stub.php` | Authoritative PHP stub (maintained manually) |
| `tests/` | Pest tests (Extension, Pool, Publisher, Consumer, Config, etc.) |
| `tests/phpt/` | PHPT tests (run via `run-tests.php`) |
| `tests/fixtures/fpm/` | FPM config for the isolation test |

**Key commands:**
```bash
cargo build -p rabbit-rs-php --features extension-tests   # debug build
./scripts/test-extension.sh                               # Pest + PHPT
./scripts/install.sh --release                            # install into PHP
```

### `packages/laravel-queue/` — Laravel bridge

Pure PHP package (`goopil/rabbit-rs-laravel`). Uses Pest for tests.

| Path | Role |
|------|------|
| `src/Connectors/` | `RabbitMqConnector` — queue connector |
| `src/Queue/` | `RabbitMqQueue` — push, pop, later, bulk, size, clear |
| `src/Jobs/` | `RabbitMqJob` — job wrapper around native Delivery |
| `src/Console/` | `rabbit-rs:work` and `rabbit-rs:status` commands |
| `src/Octane/` | Octane lifecycle hooks (flush, reload, stop) |
| `src/Config/` | `ConfigNormalizer` — maps Laravel config to native config |
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

`cargo php install` and `cargo php stubs` fail at the workspace root because the root `Cargo.toml` is a workspace manifest, not a package manifest. `cargo-php` (v0.1.11) does not resolve workspace members automatically.

The wrapper scripts pass `--manifest crates/rabbit-rs-php/Cargo.toml` under the hood:

- `./scripts/install.sh` → wraps `cargo php install`
- `./scripts/stubs.sh` → wraps `cargo php stubs`

### Stub generation

`cargo php stubs` requires the PHP embed SAPI to introspect the extension. Homebrew PHP (`php@8.4`) does not include embed by default, so `./scripts/stubs.sh` may abort with SIGABRT (exit 134) on macOS.

The authoritative stub is `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, maintained manually and validated by `php -l` and PHPT reflection tests. To regenerate stubs via `cargo php stubs`, build PHP with `--enable-embed` or use a Docker image that ships the embed SAPI.

## Test strategy

### Rust core tests

- **Unit tests** live next to their modules (`#[cfg(test)]` blocks).
- **Integration tests** live in `crates/rabbit-rs-core/tests/` (6 files).
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

### Test scripts and `php -n`

All test scripts use `php -n` to ignore system ini files. This prevents "Module already loaded" warnings when the extension is installed system-wide (e.g. via `./scripts/install.sh`).

- `php -n` = ignore all ini files
- `php -n -d extension=<artifact>` = ignore ini files, load only the local build
- Built-in extensions (curl, pcntl, json, etc.) remain available with `-n` because they are compiled into PHP

The shared helpers in `scripts/lib-extension.sh` encapsulate this:

| Function | What it does |
|----------|-------------|
| `ext_artifact_path()` | Resolves `target/debug/` or `target/release/` artifact |
| `ext_ensure_built()` | Builds with `--features extension-tests` if missing |
| `ext_verify_loads()` | Verifies the extension loads via `php -n -d extension= -m` |
| `ext_php_cmd()` | Echoes `php -n -d extension=<artifact>` |
| `ext_php_no_ext_cmd()` | Echoes `php -n` (no extension at all) |

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

# Run tests with only the local extension loaded (no system ini)
php -n -d extension=target/debug/librabbit_rs_php.dylib vendor/bin/pest
```

The `-n` flag ensures the extension is loaded exactly once. Without it, if the extension is also installed system-wide, PHP emits "Module already loaded" warnings.

### Pattern 2: Tests that must run without the extension

```bash
# Run with no ini files and no extension
php -n vendor/bin/pest
```

This is required for Laravel Unit/Feature tests because:
1. They use fake classes that simulate the extension's API
2. `RabbitMqServiceProviderTest` asserts that the provider throws when the extension is missing
3. Loading the real extension would conflict with the fake classes

### Pattern 3: System-wide installation

```bash
# Install the extension into the current PHP
./scripts/install.sh --release

# Now any PHP invocation loads it automatically
php -m | grep rabbit_rs
```

After installation, the extension is loaded from the system ini directory. Test scripts still use `php -n` to avoid double-loading.

## Common pitfalls

### `cargo php install` fails at the workspace root

**Cause:** `cargo-php` cannot resolve workspace members.

**Fix:** Use `./scripts/install.sh` which passes `--manifest crates/rabbit-rs-php/Cargo.toml`.

### `cargo php stubs` aborts with exit 134 on macOS

**Cause:** Homebrew PHP does not include the embed SAPI.

**Fix:** The stub is maintained manually at `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`. To regenerate, build PHP with `--enable-embed` or use a Docker image with the embed SAPI.

### Tests fail with "Module already loaded" warnings

**Cause:** The extension is installed system-wide and the test script also loads it via `-d extension=`.

**Fix:** Test scripts use `php -n` to ignore system ini files. If you write a new script, source `scripts/lib-extension.sh` and use `ext_php_cmd()` or `ext_php_no_ext_cmd()`.

### Laravel Unit tests fail when the extension is loaded

**Cause:** Unit/Feature tests use fake classes that conflict with the real extension classes.

**Fix:** Run without the extension: `./scripts/test-laravel.sh` (no `--with-extension` flag).

### `test-fpm.sh` says "extension artifact not found"

**Cause:** FPM tests need a release build, not debug. Or you haven't built at all.

**Fix:** `cargo build --release -p rabbit-rs-php --features extension-tests` or use `ext_ensure_built` from `lib-extension.sh` which checks both `target/debug/` and `target/release/`.

### Rust tests hang or fail with connection errors

**Cause:** The test requires the RabbitMQ lab but it's not running.

**Fix:** Start the lab: `./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh`. Or run only the unit tests: `cargo test -p rabbit-rs-core` (without `--features integration`).

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

1. `./scripts/check.sh` passes cleanly (fmt + clippy + test + composer validate)
2. `cargo fmt --all` applied after Rust edits
3. Commits are logical and scoped — no build artifacts, `.air/`, or IDE metadata
4. If you changed behavior, update the relevant doc in `docs/`
5. If you completed a planned task, update the implementation plan in `docs/plans/`
