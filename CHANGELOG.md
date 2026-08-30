# Changelog

All notable changes to the Rabbit RS workspace (native extension and Laravel bridge) are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

Releases `v0.0.1` and `v0.0.2` predate this changelog; their tags remain available in the repository.

## [Unreleased]

### Changed

- Aligned every extension version claim to the workspace version (`0.0.x`): the Laravel package requires `ext-rabbit_rs ^0.0`, matching exception messages and documentation. Introduced this changelog.

## [0.0.7] - 2026-08-26

### Added

- Homebrew distribution: formula update script, formula PR test workflow, and Homebrew update/test jobs in the release pipeline.
- Coverage pipeline: Rust (cargo-llvm-cov), PHP extension (`-Cinstrument-coverage`), and Laravel (PCOV) with Codecov upload.
- Supply-chain checks via `cargo-deny` (advisories, licenses, bans, sources).
- Horizon integration in the Laravel bridge (`worker=horizon` profile, `Horizon\RabbitMqQueue`/`Horizon\RabbitMqJob` with event dispatching and `deleteReserved`).

### Fixed

- Homebrew tap trust, token handling (`HOMEBREW_TAP_TOKEN`), and idempotent formula PR re-runs in CI.
- Coverage toolchain: rustup LLVM tools for profdata to match Rust 1.96, license allowlist and source registry for `cargo-deny`.

## [0.0.6] - 2026-08-23

### Added

- `drainSettlementErrors` in `pop()` surfaces asynchronous acknowledgement errors.
- `no_ack` transport mode gated behind `best_effort` + `early_ack`.

### Changed

- Performance: fire-and-forget settlement with bounded backpressure and error queue; `wait_all()` replaces sequential waiter polling; `TaggedFuture` eliminates a double `BoxFuture` allocation per publish; `Arc<str>` fields on `Destination`/`MessageProperties` and `Arc<Headers>` on deliveries to avoid deep clones.

### Fixed

- Backpressure hard gate stops accepting deliveries when `max_buffered_bytes` is exceeded.
- Delivery budget and ledger are only released on success or terminal settlement failures.
- `try_next_batch` returns a partial batch on error instead of discarding deliveries.
- Coordinators start for all distinct brokers in multi-broker worker profiles.
- Generation-aware handle invalidation; recovery generation is rolled back on failure and the old consumer is closed on replacement.
- Benchmark fairness: `AUTO_ACK` runs with `confirms=false`, `basic_consume` migration, prefetch 128.

## [0.0.5] - 2026-08-23

### Changed

- Documentation for macOS ARM64 pre-compiled binaries and from-source builds.

### Fixed

- Release asset names prefixed with `v` for PIE compatibility.
- Packagist API submission format (username + API token).

## [0.0.4] - 2026-08-23

### Added

- macOS NTS builds in the release pipeline.
- Dependabot configuration for Cargo, Composer, and GitHub Actions.

### Changed

- Laravel mirror split sequenced after native release publication.

## [0.0.3] - 2026-08-23

### Added

- Early-ACK best-effort mode with a Laravel reliable-mode guard.

### Changed

- Configuration defaults: prefetch raised to 64 and `max_in_flight` to 256.

### Fixed

- Worker supervisor stops all children on `max-restarts` and propagates worker options.
- `status` command returns a non-zero exit code and logs an error when stats collection fails.
- Queue type and durability from configuration are passed to the native transport.
- `message_id` and JSON payload are validated before job creation to prevent redelivery loops.
- Pools are closed before clearing the cache in `flush` and `resetAfterFork`.
- `delivery_limit` without `dead_letter` is rejected to prevent silent message loss.
- Linux builds: version-script linker fixes; Pest v4 upgrade for Laravel 13 support.

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...HEAD
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
