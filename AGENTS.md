@/Users/zacharyvolpi/.codex/RTK.md

# Rabbit RS Repository Guide

## Project Overview

Rabbit RS is a high-performance RabbitMQ transport for PHP and Laravel, powered by Rust. The workspace contains a runtime-independent Rust core, a native PHP extension, and a Laravel queue driver package.

The delivery contract is at-least-once: silent loss is unacceptable, while duplicates are permitted and must remain identifiable and measurable.

## Sources of Truth

- `docs/plans/2026-07-30-rabbitmq-native-design.md` defines the approved product architecture, supported platforms, delivery semantics, and operational constraints.
- `docs/plans/2026-07-30-rabbitmq-native-implementation.md` defines the task sequence, current milestone status, and expected file layout.
- Read the relevant plan sections before changing behavior. Keep milestone details in those documents instead of duplicating them here.

## Workspace Map

- `crates/rabbit-rs-core/`: runtime-independent configuration, connection pooling, topology, publishing, consuming, recovery, metrics, and transport abstractions.
- `crates/rabbit-rs-core/tests/`: consolidated Rust integration tests (6 files: publisher, consumer, recovery, topology, metrics, integration).
- `crates/rabbit-rs-php/`: `cdylib` for the native PHP extension; depends on the core crate. Pest tests in `tests/`.
- `packages/laravel-queue/`: Laravel queue driver package (`goopil/rabbit-rs-laravel`). Pest tests in `tests/`.
- `benchmarks/`: PHP benchmark suite with AbstractBenchmark pattern, 4 drivers, 3 scenarios.
- `composer.json`: PIE package metadata for `rabbit-rs/native`.
- `scripts/check.sh`: Rust quality gate (fmt + clippy + test + composer validate).
- `scripts/lib-extension.sh`: shared helpers for building and loading ext-rabbit_rs in test scripts.
- `scripts/test-laravel.sh`: run Laravel Pest tests (Unit + Feature without extension, Integration with extension).
- `scripts/test-extension.sh`: build and test the PHP extension (Pest + PHPT).
- `scripts/test-integration.sh`: run integration tests with RabbitMQ lab (Rust + Laravel Integration).
- `scripts/test-fpm.sh`: FPM multi-worker pool handle isolation test.
- `scripts/test-octane.sh`: Octane lifecycle tests (fake classes, no extension).

## Toolchain and Commands

- Rust is pinned to 1.96.0 and uses edition 2024.
- Run focused checks while iterating:
  - `rtk cargo test -p rabbit-rs-core config::tests`
  - `rtk cargo test -p rabbit-rs-core --test publisher_safety`
  - `rtk cargo test -p rabbit-rs-core`
- Run individual quality checks when diagnosing failures:
  - `rtk cargo fmt --all -- --check`
  - `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `rtk cargo test --workspace --all-targets`
  - `rtk composer validate --strict`
- Before claiming completion, run the complete gate: `rtk ./scripts/check.sh`.

## PHP Extension Tooling

- `cargo php install` and `cargo php stubs` fail at the workspace root because the root `Cargo.toml` is a workspace manifest, not a package manifest. `cargo-php` (v0.1.11) does not resolve workspace members automatically.
- Use the wrapper scripts instead:
  - `./scripts/install.sh [--release] [--yes]` — builds and installs the extension into the current PHP.
  - `./scripts/stubs.sh [--stdout] [-o <path>]` — generates PHP stubs from the compiled extension.
- Both scripts pass `--manifest crates/rabbit-rs-php/Cargo.toml` to `cargo-php` under the hood.
- `cargo php stubs` requires the PHP embed SAPI to introspect the extension. Homebrew PHP (`php@8.4`) does not include embed by default, so `./scripts/stubs.sh` may abort with SIGABRT (exit 134) on macOS. The authoritative stub is `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, maintained manually and validated by `php -l` and PHPT reflection tests.
- To regenerate stubs via `cargo php stubs`, build PHP with `--enable-embed` or use a Docker image that ships the embed SAPI.

## Extension Loading in Test Scripts

- Test scripts use `php -n` to ignore all system ini files, preventing "Module already loaded" warnings when the extension is installed system-wide.
- `scripts/lib-extension.sh` provides shared helpers:
  - `ext_artifact_path()`: resolves `target/debug/librabbit_rs_php.{dylib|so}` (falls back to `target/release/`).
  - `ext_ensure_built()`: builds the extension with `--features extension-tests` if the artifact is missing.
  - `ext_php_cmd()`: echoes `php -n -d extension=<artifact>` for tests that need the extension.
  - `ext_php_no_ext_cmd()`: echoes `php -n` for Unit/Feature tests that must run without the extension.
- Built-in extensions (curl, pcntl, json, etc.) remain available with `-n` because they are compiled into PHP.
- Laravel Unit/Feature tests must run without the extension so the "missing extension" assertion in `RabbitMqServiceProviderTest` passes.

## Rust Conventions

- Unsafe Rust is forbidden. Do not weaken `#![forbid(unsafe_code)]` or the workspace lint configuration.
- Keep Lapin behind the `Transport` abstraction so broker behavior remains mockable and replaceable.
- Prefer typed errors with actionable context over strings; configuration failures must identify their exact input path.
- Document public APIs and retain useful `#[must_use]` annotations.
- Keep queues, channels, in-flight work, retries, and replay buffers explicitly bounded.
- Never expose credentials, complete broker URIs, or private certificate material through `Debug`, errors, metrics, or logs.
- Do not retain Zend values, PHP objects, callbacks, requests, or service-container state in Rust threads.

## Reliability Invariants

- Unconfirmed publications survive connection recovery only in bounded process memory and are replayed with the same `message_id` and original deadline.
- Never describe in-memory replay as durable across a PHP process crash; durability beyond a crash requires an external outbox.
- Publisher confirms, mandatory returns, timeouts, and terminal errors resolve each waiter once. A mandatory return takes precedence over its following ACK.
- Runtime and connection registries are lazy and process-local. A PID change invalidates inherited resources after a fork.
- A vhost owns a distinct AMQP connection. Consumer channels remain dedicated; publisher channels may be pooled.
- Delivery tokens and acknowledgements are connection-generation-aware. Stale ACKs must be rejected so RabbitMQ can redeliver.
- Recovery order remains deterministic: connection, channels, exchanges, queues, bindings, QoS, then consumers.

## Testing and Workflow

- Follow test-driven development for behavior changes: add a focused failing test, observe the intended failure, implement minimally, and rerun the focused test.
- Use paused Tokio time and the scriptable mock transport for deterministic asynchronous tests. Do not add real sleeps to unit tests.
- Add cross-module behavior tests under `crates/rabbit-rs-core/tests/`; keep private unit details next to their modules.
- PHP tests use Pest (not PHPUnit). Laravel Unit/Feature tests use fake classes (no extension needed). Integration tests require ext-rabbit_rs loaded.
- Run `rtk cargo fmt --all` after Rust edits, then run focused tests and the full quality gate.
- Preserve unrelated work in a dirty tree. Never discard or overwrite changes you did not create.
- Keep commits logical and scoped when the active plan calls for commits. Do not include `.air/`, IDE metadata, build artifacts, or unrelated changes.
- Update the implementation plan when completing a planned task so its progress and next step stay accurate.
