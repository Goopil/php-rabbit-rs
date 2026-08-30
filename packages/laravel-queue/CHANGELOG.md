# Changelog

All notable changes to `goopil/rabbit-rs-laravel`, the Laravel queue driver for the Rabbit RS native extension. This is a simplified mirror of the [workspace changelog](https://github.com/Goopil/rabbit-rs/blob/main/CHANGELOG.md); releases are synchronized with the native extension (`goopil/rabbit-rs-native`).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

## [Unreleased]

### Changed

- The `ext-rabbit_rs` requirement is documented as `^0.0` everywhere (composer, exception message, docs), aligned with the extension workspace version.

## [0.0.7] - 2026-08-26

### Added

- Horizon integration: `worker=horizon` profile with `Horizon\RabbitMqQueue` (event dispatching) and `Horizon\RabbitMqJob` (`deleteReserved` support), dynamic class resolution in the connector, and a `laravel/horizon` suggestion.

## [0.0.6] - 2026-08-23

### Added

- `drainSettlementErrors` in `pop()` surfaces asynchronous acknowledgement errors.
- `no_ack` transport mode gated behind `best_effort` + `early_ack`.

## [0.0.5] - 2026-08-23

Packaging-only release: no Laravel bridge changes.

## [0.0.4] - 2026-08-23

Packaging-only release: Laravel mirror split sequenced after native release publication.

## [0.0.3] - 2026-08-23

### Added

- Early-ACK best-effort mode with a reliable-mode guard.

### Changed

- Configuration defaults: prefetch raised to 64 and `max_in_flight` to 256.

### Fixed

- Worker supervisor stops all children on `max-restarts` and propagates worker options.
- `status` command returns a non-zero exit code and logs an error when stats collection fails.
- Queue type and durability from configuration are passed to the native transport.
- `message_id` and JSON payload are validated before job creation to prevent redelivery loops.
- Pools are closed before clearing the cache in `flush` and `resetAfterFork`.
- `delivery_limit` without `dead_letter` is rejected to prevent silent message loss.

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...HEAD
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
