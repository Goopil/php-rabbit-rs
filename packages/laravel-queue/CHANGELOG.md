# Changelog

All notable changes to `goopil/rabbit-rs-laravel`, the Laravel queue driver for the Rabbit RS native extension. This is a simplified mirror of the [workspace changelog](https://github.com/Goopil/rabbit-rs/blob/main/CHANGELOG.md); releases are synchronized with the native extension (`goopil/rabbit-rs-native`).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

## [Unreleased]

### Fixed

- Require `ext-rabbit_rs ^0.1` (was `^0.0`): the 0.1.0 extension no longer satisfies a caret constraint pinned to 0.0.x, which made the package uninstallable (`composer check-platform-reqs` failure) wherever the current extension was loaded. The constraint, the `RabbitMqServiceProvider::EXTENSION_CONSTRAINT` message constant, and `docs/troubleshooting.md` are aligned, and a unit test now fails when the workspace crate version moves without the requirement following.

## [0.1.0] - 2026-09-02

### Added

- Connection-first configuration: every broker, its credentials, routes, and consumer profile live on a single `queue.connections.*` connection in `config/queue.php` (the SQS/redis idiom); `config/rabbit-rs.php` shrinks to ~50 lines of cross-cutting defaults merged under every rabbit-rs connection (per sub-key for `tls`, `delay`, `dead_letter`). There is **no compatibility shim** for the old shape (pre-1.0 break; see the migration table below).
- Lazy per-connection compilation at `connect()`: a config typo only fails the queue driver's use instead of crashing the whole application at boot, Laravel env strings (`'1'`, `'true'`, `'on'`, `"64"`, …) are cast inside the driver, and every error carries the exact `queue.connections.<name>.<key>` path.
- `rabbit-rs:work` fan-out: with no flags it consumes every queue defined on every rabbit-rs connection (one supervised `queue:work` child per connection); `--connection=a,b` targets connections explicitly; `--queue=x,y` resolves names **by definition** (the connection's `queue` key or a `subscriptions` alias) with a typed error listing available names; a queue defined on two targeted connections is consumed on both; `--workers` now spawns children per connection.
- Two queue connections with byte-identical arrays compile to the same fingerprint and share one native pool per process.
- `octane:reload` forgets the queue manager's resolved connections, so the next request recompiles each connection from the current config — broker/credential rotation via env takes effect without stale brokers.

### Changed

- **Breaking:** the old `config/rabbit-rs.php` namespaces (`brokers.*`, `routes.*`, `workers.*`) are gone. Migrate your config:

  | Old key (`config/rabbit-rs.php`) | New location |
  |---|---|
  | `brokers.<b>.hosts` / `.credentials` / `.tls` / `.heartbeat` | connection `hosts` / `username` + `password` / `tls` / `heartbeat` |
  | `routes.<q>.exchange` / `.routing_key` | connection `exchange` / `routing_key` (`{queue}` placeholder unchanged) |
  | `workers.<w>.subscriptions.*` | connection `subscriptions` escape hatch; without it, one subscription is derived from the connection's `queue` |
  | `workers.<w>` profile targeting | `rabbit-rs:work --connection=<name>` |
  | `publisher.confirms` / `publisher.mandatory` | `safety` only (`safe`/`unsafe`/`blind` derive confirms and mandatory) |
  | `scheduler.strategy` | deleted — dead knob (a single strategy existed) |
  | `prefetch.mode` | deleted — dead knob |
  | `brokers.<b>.management_url` (env `RABBIT_RS_MANAGEMENT_URL`) | connection key `management_url`, read by `rabbit-rs:status` |
  | `consumers.max_attempts` (env `RABBIT_RS_MAX_ATTEMPTS`) | connection key `max_attempts` (default 20) |
  | `routes.default.exchange` (env `RABBIT_RS_EXCHANGE`) | connection key `exchange` (default `laravel.jobs`) |
  | `workers.default.subscriptions.default.queue` (env `RABBIT_RS_QUEUE`) | connection key `queue` (required, no default) |

  The env hooks `RABBIT_RS_MAX_ATTEMPTS`, `RABBIT_RS_EXCHANGE`, and `RABBIT_RS_QUEUE` are silently dropped from the package config — those values now live as plain connection keys (`max_attempts`, `exchange`, `queue`); wire them with `env()` directly on the connection in `queue.php` if you need an env hook (see the package's [configuration guide](https://github.com/Goopil/rabbit-rs/blob/main/docs/configuration.md)). The same applies to the former broker env hooks `RABBIT_RS_HOSTS`/`RABBIT_RS_VHOST`/`RABBIT_RS_USERNAME`/`RABBIT_RS_PASSWORD`.
- `hosts` strings with empty segments (e.g. `"host1:5672,,host2"`) are now strictly rejected with a typed config error instead of silently skipping the empty segment; at least one host is required.
- Unknown keys — on the connection or inside `tls`, `delay`, `dead_letter`, `subscriptions` — are rejected with their full config path; strictness is otherwise unchanged (`dead_letter` required with `delivery_limit`, `no_ack` requires `early_ack` + `best_effort`, range checks).

## [0.0.9] - 2026-09-01

### Fixed

- Event callbacks: `RabbitMqQueue` clears existing event callbacks before registering its defaults, so worker/pool reuse no longer accumulates duplicate default callbacks (each native event firing once per queue construction on a shared pool). To override the defaults, call `Pool::clearEventCallbacks()` before registering a custom callback.
- Worker supervisor: clean child exits (exit 0, e.g. `--max-jobs` or `--max-time` recycling) no longer burn the restart budget — the budget resets and the worker restarts immediately without backoff; crash-loop protection (budget + exponential backoff) now applies only to non-zero exits.
- Config lifecycle: a `rabbit-rs` config typo no longer crashes the whole application at boot — normalization runs when a queue connection resolves, so only the driver's use fails with the validation error. Laravel env-string booleans (`'1'`, `'0'`, `'true'`, `'false'`, `'on'`, `'off'`, `''`) are accepted in boolean fields (junk strings are still rejected with the config path).
- Octane: `octane:reload` re-normalizes the `rabbit-rs` config — broker/credential rotation via env variables now takes effect for connections resolved after the reload instead of silently serving the boot-time snapshot.
- `queue:size` and `queue:clear` (native `size()`/`clear()`) flush the publish buffer first, so they report/act on the true broker state — previously buffered publications were invisible to `size()` and could repopulate a queue right after `clear()`.

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

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Goopil/rabbit-rs/compare/v0.0.9...v0.1.0
[0.0.9]: https://github.com/Goopil/rabbit-rs/compare/v0.0.8...v0.0.9
[0.0.8]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
