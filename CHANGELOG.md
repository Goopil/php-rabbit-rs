# Changelog

All notable changes to the Rabbit RS workspace (native extension and Laravel bridge) are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

Releases `v0.0.1` and `v0.0.2` predate this changelog; their tags remain available in the repository.

## [Unreleased]

### Added

- `Pool::clearEventCallbacks()`: removes every registered event callback (connection-state and backpressure combined) and returns how many were removed, so a connection sharing a native pool can re-register from a clean slate.
- Conservative GC for synthesized TTL delay queues: `topology::delay::sweep_delay_queues()` (Rust core) deletes orphaned `rabbit-rs.delay.*` queues through an admin channel once a rolling deploy completes. It only touches the reserved `rabbit-rs.delay.*` prefix, only queues of destinations it is given, keeps every name the current plan still produces and every queue still draining messages, and re-probes emptiness immediately before each delete (quorum queues reject `if-empty`/`if-unused` deletes).

### Changed

- TTL delay-queue names now bind the declaring arguments: `rabbit-rs.delay.{destination}.{args}.{bucket}` where `{args}` fingerprints `x-message-ttl`, `x-expires` and the dead-letter target. Two configurations with different buckets or `queue_expiry_margin` values therefore declare distinct queues instead of fighting over one name — `PRECONDITION_FAILED` (406) storms during rolling deploys are gone. Queues synthesized by the previous naming scheme drain through their own message TTL and dead-letter exchange and are eventually deleted by their `x-expires`; the new sweep accelerates the cleanup.
- `Pool::onConnectionState()` and `Pool::onBackpressure()` now register multiple callbacks instead of replacing the previous one, so connections sharing one native pool (e.g. two Laravel connections with the same fingerprint) each keep their own callbacks.

### Fixed

- Exceptions thrown inside `onConnectionState`/`onBackpressure` callbacks are no longer silently destroyed: the original exception object is rethrown once the event drain finishes (when the enclosing operation itself fails, the callback exception is preserved in the `$previous` chain of the surfaced error).
- Consumer `next()`, `tryNext()`, and `nextBatch()` drain native events on every call — previously the drain only ran when the delivery buffer was empty, so state/backpressure callbacks starved under steady traffic and dashboards showed healthy state during incidents.

## [0.0.8] - 2026-08-31

### Added

- Consumer acquisition deadline: `consumer.wait_timeout` (default 30 s, validated 1 s–24 h) with a typed transport error at expiry, so FPM workers can no longer freeze against a black-holed broker; Laravel `consumers.wait_timeout` passthrough.
- Publish buffer backpressure in the extension: the buffer is bounded (4096 messages / 64 MiB) and raises an explicit `BackpressureException` when full and unable to flush; already-accepted messages are never dropped.
- Poison-message warning: a production warning is emitted once per process when `delivery_limit` and `dead_letter` are both unset (infinite redelivery for worker-crashing messages); opt-out via `production_warning => false` (per connection, package config, or `RABBIT_RS_PRODUCTION_WARNING`).
- Laravel contracts: `RabbitMqQueue` implements `ClearableQueue` (`clear(): int` returns the purge count for `queue:clear`), and `pop()` resolves plain queue names via opt-in `auto_subscribe` (connection > package > `RABBIT_RS_AUTO_SUBSCRIBE`).
- Native events (`ConnectionStateChanged`, `BackpressureDetected`) now drain from `publish()`, `publishBatch()`, `flush()` and `next()` through a shared `EventBridge` — no longer only inside `stats()`.
- Duplicate subscription names within a worker profile are rejected with a typed configuration error identifying the exact path.
- PIE end-to-end delivery validation: unified `-nts` naming convention across the release pipeline and a blocking `verify-pie-install` CI job that runs a real `pie install` against the release, gating the Homebrew formula update and the Laravel package split.

### Changed

- Horizon: `push`/`later` honor `after_commit` through `enqueueUsing` (transactional jobs no longer publish before the SQL commit) and `bulk()` routes through prepared `prepareBatch`/`publishBatch` overrides so bulk jobs surface in the dashboard with `JobPending`/`JobPushed` events.
- Recovery establishes only the requested consumer profiles; a profile first requested after a publishing phase is established on demand (publisher-only processes no longer retain unacked messages on unrelated queues at reconnection).
- Worker supervisor: the pcntl-free `--workers=1` path runs inline with the same backoff/max-restarts semantics, and restart backoff is non-blocking (other children keep being supervised during a backoff window).
- ZTS dropped from the V1 release matrix (16 → 8 assets): `support-zts: false` until thread isolation, a blocking ZTS CI job, and real concurrency tests land in V2.
- Extension version claims aligned on `^0.0` / workspace `0.0.x` until 1.0; workspace and package CHANGELOGs introduced.

### Fixed

- Consumer stall under sustained pop+ack and pre-fill missing deliveries (~2%): the publish buffer is now flushed on the consume path — one root cause, both symptoms. Round 2 re-bench: worker pop+ack +117% vs the taxed baseline (21 747 ops/s median), 0 losses, 0 stall recoveries across 30 rounds (archive: `benchmarks/results/round-2-rebench/`).
- Unreadable TLS certificate files fail loudly with a typed error identifying the exact path instead of silently connecting without the configured CA.
- Round 2 secondary scope: closed-pump batch contract pinned (immediate failure + re-buffer, superset semantics), non-vacuous `flush_blind` flush test, and `scripts/lib-extension.sh` rebuild-on-change so stale extension artifacts are rebuilt automatically instead of silently reused.

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

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.0.8...HEAD
[0.0.8]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
