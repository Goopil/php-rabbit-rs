# Changelog

All notable changes to `goopil/rabbit-rs-laravel`, the Laravel queue driver for the Rabbit RS native extension. This is a simplified mirror of the [workspace changelog](https://github.com/Goopil/rabbit-rs/blob/main/CHANGELOG.md); releases are synchronized with the native extension (`goopil/rabbit-rs-native`).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

## [Unreleased]

### Fixed

- Worker supervisor: clean child exits (exit 0, e.g. `--max-jobs` or `--max-time` recycling) no longer burn the restart budget — the budget resets and the worker restarts immediately without backoff; crash-loop protection (budget + exponential backoff) now applies only to non-zero exits.
- Config lifecycle: a `rabbit-rs` config typo no longer crashes the whole application at boot — normalization runs when a queue connection resolves, so only the driver's use fails with the validation error. Laravel env-string booleans (`'1'`, `'0'`, `'true'`, `'false'`, `'on'`, `'off'`, `''`) are accepted in boolean fields (junk strings are still rejected with the config path).
- Octane: `octane:reload` re-normalizes the `rabbit-rs` config — broker/credential rotation via env variables now takes effect for connections resolved after the reload instead of silently serving the boot-time snapshot.

## [0.0.8] - 2026-08-31

### Added

- `ClearableQueue` support: `queue:clear` works and reports the purge count (`clear(): int`).
- Opt-in `auto_subscribe` (connection > package > `RABBIT_RS_AUTO_SUBSCRIBE`): `pop()` resolves plain queue names through implicit `__auto__.<queue>` profiles cached per queue; worker-profile names keep working.
- Production warning when `delivery_limit` and `dead_letter` are both unset (infinite redelivery for worker-crashing messages), opt-out via `production_warning => false`.
- Horizon `bulk()` now prepares payloads and fires `JobPending`/`JobPushed`, so bulk jobs are visible in the dashboard.
- `consumers.wait_timeout` passthrough (ms, `RABBIT_RS_CONSUMER_WAIT_TIMEOUT`) mapping to the native `consumer.wait_timeout` acquisition deadline.

### Changed

- Horizon `push`/`later` honor `after_commit` through `enqueueUsing` — transactional jobs publish only after the SQL commit.
- Worker supervisor: pcntl-free `--workers=1` runs inline, and restart backoff is non-blocking.
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

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.0.8...HEAD
[0.0.8]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
