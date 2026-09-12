# Changelog

All notable changes to `goopil/rabbit-rs-laravel`, the Laravel queue driver for the Rabbit RS native extension. This is a simplified mirror of the [workspace changelog](https://github.com/Goopil/rabbit-rs/blob/main/CHANGELOG.md); releases are synchronized with the native extension (`goopil/rabbit-rs-native`).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

## [0.3.1] - 2026-09-12

### Fixed

- Requires `ext-rabbit_rs ^0.3.1` (lockstep release). No Laravel-layer change;
  the native extension is unchanged — this release exists so the release
  pipeline's PIE install verification passes (#243).

## [0.3.0] - 2026-09-12

### Changed

- **BREAKING** — `auto_subscribe` is rejected at connection-compile time
  (#228): the option could only surface the native `unknown worker profile`
  error at first pop. Remove the key from your connection config (including
  `RABBIT_RS_AUTO_SUBSCRIBE`, which is gone from the package config); declare
  queues explicitly with the connection `queue` key or the `subscriptions`
  escape hatch.
- Requires `ext-rabbit_rs ^0.3.0` (lockstep release; `tls.verify: none` is
  no longer a valid value — the only accepted value is `peer`).

## [0.2.3] - 2026-09-12

### Fixed

- Requires `ext-rabbit_rs ^0.2.3` (lockstep release). No Laravel-layer change;
  the native extension is now installable through PIE on macOS Apple Silicon
  (`pie install goopil/rabbit-rs-native`), validated by the release pipeline on
  a macOS arm64 runner.

## [0.2.2] - 2026-09-11

### Fixed

- Requires `ext-rabbit_rs ^0.2.2` (lockstep release): the native extension now
  enforces the publish buffer's `flush_interval` age deadline with a background
  timer, so a lone publish (or the tail of a burst) reaches the broker within
  the configured interval even when the process never publishes, pops, or
  flushes again — previously a process that stopped publishing held its batch
  in memory until the next operation or close (a lone FPM publish stayed
  invisible; alternating publishes landed in pairs). No Laravel-layer change;
  the package and the extension move together.

## [0.2.1] - 2026-09-10

### Fixed

- Require `ext-rabbit_rs ^0.2.1` (was `^0.2`): the compiled config now always
  carries the publish `routes` inside the native section, and the 0.2.0 core
  config (deny_unknown_fields, no `routes` field) rejects the unknown key at
  pool creation. The package and the extension must move together.
- The topology compiler declares the connection's publish route (#205): the compiled `routes` map (mirrored inside `native`) has `rabbit-rs:topology --fix` and declare-mode boot declare the exchange and its per-subscription `{queue}` bindings, so queues are reachable for publishers and delay-bucket and dead-letter republishing reach the main queue. Connections publishing through the default exchange (`exchange => null`) need no declaration.
- Pops on multi-queue profiles are scoped regardless of `auto_subscribe` (#207): a `pop()` addressed to one queue of a multi-queue connection always resolves a dedicated `__auto__.{queue}` consumer, so default multi-queue connections (and Horizon supervisors popping named queues) no longer draw jobs from other queues. `auto_subscribe` keeps its remaining meaning: allowing pops on queues not defined in any profile.
- Rejected `delivery_limit` on classic queues and non-durable quorum queues at compile time (#204): both combinations fail broker-side with `precondition_failed`, so `ConnectionCompiler` now throws with the exact `queue.connections.<name>.<key>` path before the extension is ever loaded.
- `rabbit-rs:topology --fix` no longer blocks on the consumer readiness gate when no worker is running (#208, #214): the declare probe runs on a copy of the native config whose consumer readiness wait is bounded to 2 s (`DoctorProbe::declareConfig`), surfacing the bootstrap soft warning instead of stalling ~30 s.
- `rabbit-rs:topology` verify treats an unreachable broker as unverifiable instead of failing queues as missing (#208): the passive queue probe reports a warn; NOT-FOUND still fails with the config path.

## [0.2.0] - 2026-09-10

Breaking release — upgrade the package and the native extension together.

### Changed

- **Breaking** — subscriptions are weight-only: `priority_class` strict preemption and its `starvation_after` aging compensation are removed; carrying either key is rejected with the exact config path.
- **Breaking** — the connection-level `delay_mode` scalar is gone: delay configuration now lives in the package config as `delay` (`mode`, `buckets`, `max_buckets`, `queue_expiry_margin`), merged per sub-key under every connection and overridable per connection.
- **Breaking** — the native extension requirement moves to `^0.2` (was `^0.1`): upgrade the package and the extension together. Running this package on a 0.1.x extension fails with `unknown field 'flush_interval'` — a native config field the 0.1.x extension does not know.
- Pops on multi-queue profiles are scoped (#183): with `auto_subscribe` enabled, a `pop()` addressed to one queue of a multi-queue connection resolves a dedicated `__auto__.{queue}` consumer instead of the shared profile — prefetch and deliveries are no longer pooled across queues.
- `Pool::close()` drains before dropping (#194): pending buffered publications are attempted on the wire and awaited within the publisher confirm timeout before channels close; only what genuinely cannot be attempted fails loudly (counted in `dropped_publications_total`).

### Added

- `publisher.flush_interval` (#194): the publish buffer's age-flush latency knob (integer ms, default `1`, bounded 0..3,600,000).

### Fixed

- Delayed delivery is honest across every publish path (#196): a delay the compiled strategy cannot route fails terminally instead of publishing to the original exchange, where the ignored `x-delay` header would run the job immediately.

## [0.1.6] - 2026-09-08

### Added

- Kubernetes probes (#85): the worker writes a per-PID JSON statefile and `rabbit-rs:probe {startup|ready|alive|prestop}` evaluates it; the `RabbitRsProbeEvaluated` event lets synchronous listeners force a verdict.
- `rabbit-rs:work --stop-when-empty` (#185): children run once and are never recycled; the supervisor exits with the highest child exit status (CI pipelines, pop-once tooling).

### Changed

- `ext-rabbit_rs` moves from `require` to `suggest` (#58): `composer install` no longer hard-fails without the native extension; resolving a rabbit-rs connection raises a precise runtime error with install instructions instead.
- `wait_timeout` is documented as the transport acquisition deadline, not the pop wait (#184): the pop wait is the standard `block_for` connection key, honored end-to-end.

### Fixed

- Doctor: the broker probe no longer reports "Undefined variable $nativeConfig" on healthy brokers; the Horizon alignment check reads the real `environments.<env>.supervisor-<name>` config shape and only counts supervisors bound to the checked connection; the contradictory worker "inheritance trap" warning is dropped (#186).
- `rabbit-rs:topology --fix` reports success on the declare step itself, and consumer-profile readiness downgrades to a warning instead of failing the bootstrap scenario (#195).
- `size()` and `clear()` force-flush the publish buffer before reading, restoring the read-after-dispatch contract (#194).

## [0.1.5] - 2026-09-07

### Added

- Core synthesizes default worker profiles for `__auto__.{queue}` names at first pop: the `auto_subscribe` path works after pool creation without config mutation.

### Changed

- Documentation restructured into getting-started tracks plus one `reference.md` per track; the Laravel driver reference now lives under `packages/laravel-queue/docs/`.

## [0.1.4] - 2026-09-06

### Fixed

- `rabbit-rs:status` always reported zeros for the management-API queue counters: the command now reads the nested `message_stats` object (`deliver_get`/`ack`/`redeliver`) and additionally exposes the current queue depth as `messages_ready`.

## [0.1.3] - 2026-09-06

### Added

- `rabbit-rs:doctor` (#155): one-shot integration diagnostics per connection; exits non-zero when a check fails.
- `rabbit-rs:topology` (#84, #165): preflight topology check for CI/deploy pipelines; `--fix` declares the missing topology.
- Adaptive prefetch per subscription (#42, #162): a `min`/`target buffer`/`max` policy adjusted at runtime by the native pool, observable via `getPrefetchStats()`.
- Native: the transport enforces TLS certificate verification and sends the SNI `server_name` to the broker.

### Fixed

- Child workers receive their index through the dedicated `RABBIT_RS_WORKER_INDEX` environment variable (#163) instead of reusing the worker-mode variable.

## [0.1.2] - 2026-09-06

Packaging/CI release: no Laravel driver changes since 0.1.1.

## [0.1.1] - 2026-09-05

### Added

- Native: safe-mode publishes are pipelined — `publish` no longer blocks on the batch flush barrier; confirmations, returns, and failures surface at the next pool operation (safe publish ×3.64 in the fresh-lab benchmark).

### Fixed

- `Horizon\RabbitMqQueue` implements `readyNow()`: Horizon's AutoScaler calls it unconditionally on every scaling pass, so any supervisor pointed at a rabbit-rs connection crash-looped with `Call to undefined method ... readyNow()` (#152).
- The `worker` cross-cutting default from `config/rabbit-rs.php` is inherited by connections that do not declare their own `worker` key (#152).
- Exhausted rabbit-rs jobs are recorded as failed in Horizon: the queue dispatches Horizon's `JobFailed` itself, which Horizon only bridges for the Redis job class (#152).
- A publication whose deadline expired while parked during a connection recovery suspension is re-armed exactly once with a fresh deadline and replayed with the same `message_id` — the first publish after an idle no longer fails with "publish deadline expired" (#151).
- `ext-rabbit_rs` requirement raised to `^0.1` (#146): the 0.1.0 extension no longer satisfied the caret constraint pinned to 0.0.x, which made the package uninstallable wherever the current extension was loaded.
- Native: a concurrent `take()` could panic with `attempt to subtract with overflow` and abort the process — the publish buffer's message list and byte accounting now mutate under one mutex.

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

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Goopil/rabbit-rs/compare/v0.1.6...v0.2.0
[0.1.6]: https://github.com/Goopil/rabbit-rs/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/Goopil/rabbit-rs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/Goopil/rabbit-rs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/Goopil/rabbit-rs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/Goopil/rabbit-rs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Goopil/rabbit-rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Goopil/rabbit-rs/compare/v0.0.9...v0.1.0
[0.0.9]: https://github.com/Goopil/rabbit-rs/compare/v0.0.8...v0.0.9
[0.0.8]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
