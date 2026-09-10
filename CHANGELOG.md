# Changelog

All notable changes to the Rabbit RS workspace (native extension and Laravel bridge) are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — while the project is pre-1.0, breaking changes may occur in minor releases.

Releases `v0.0.1` and `v0.0.2` predate this changelog; their tags remain available in the repository.

## [Unreleased]

### Fixed

- Keep-alive redeclare of live TTL bucket queues every `queue_expiry_margin`/2 (#211): the recovery coordinator passively redeclares the current plan's bucket queues so lazy quorum expiry can no longer delete a bucket queue with an in-flight delayed job; orphaned buckets keep self-cleaning through their own `x-expires`.
- The topology plan declares the publish route (#205): each configured route's exchange (direct, durable) and per-subscription `{queue}` bindings joined the core config (`routes` map), the config fingerprint, and the topology plan, making declare-mode queues reachable for publishers and letting delay-bucket and dead-letter republishing reach the main queue. Routes through the default exchange (`exchange: null`) need no declaration or binding.
- Compile-time rejection of `delivery_limit` on classic queues and of non-durable quorum queues (#204): both combinations fail broker-side with `precondition_failed`; the core config validation and the Laravel compiler now reject them with typed errors and exact paths.

## [0.2.0] - 2026-09-10

Breaking release: weight-only scheduling, plus honest delayed delivery and a loss-free terminating close from the early-adopter lab findings.

### Changed

- **BREAKING** — subscriptions are weight-only (#181): `priority_class` strict
  preemption and its `starvation_after` aging compensation are removed across
  the core config, the weighted-fair scheduler (which cannot starve by
  construction), the PHP stubs, the Laravel compiler, and the docs. Carrying
  either key is rejected with an actionable path.
- **BREAKING** — the native config schema no longer accepts
  `priority_class` / `starvation_after` in subscription objects; single-broker
  pools may now omit the subscription `broker` key entirely (pinned to the
  sole declared broker before deserialization; multi-broker pools keep the
  explicit pin mandatory) (#181).
- Pops on multi-queue profiles are scoped (#183): with `auto_subscribe`
  enabled, a pop addressed to one queue of a profile that round-robins several
  subscriptions resolves a dedicated `__auto__.{queue}` consumer instead of
  the shared profile — prefetch and deliveries are no longer pooled across
  queues. Single-queue connections, profile-name pops, and
  `auto_subscribe=false` keep the previous behavior.
- `Pool::close()` drains before dropping (#194): pending buffered publications
  are attempted on the wire and awaited within the publisher confirm timeout
  before channels close; only what genuinely cannot be attempted fails loudly
  through `Closed` (counted in `dropped_publications_total`). This fixes the
  multi-pool terminating-close tail loss (900 publishes → 0 in the lab).
- Documentation rework: the auto_subscribe section describes the scoped pop
  semantics, the subscriptions reference drops the removed knobs, and the
  README states the unknown-queues-just-work contract (#167).

### Added

- `publisher.flush_interval` (#194): documented, bounded knob (integer ms,
  default 1, 0..3,600,000) controlling the async age-flush latency of the
  publish buffer — wired core → PHP extension → Laravel compiler.

### Fixed

- Delayed delivery is honest across every publish path (#196): blind-pump and
  delayed-release publications route through the compiled delay strategy with
  lazy infrastructure declaration; a delay the strategy cannot route fails
  terminally instead of publishing to the original exchange where the ignored
  `x-delay` header would run the job immediately; flush honors the remaining
  broker-side delay; publisher acquisition is bounded by the confirm timeout
  instead of hot-spinning; the consumer delayed-release double-route drop is
  fixed.

## [0.1.6] - 2026-09-08

Feature release: Kubernetes probes, an optional native extension at install time, and worker termination control — plus fixes for the early-adopter lab findings.

### Added

- Kubernetes probes (#85): the worker writes a per-PID JSON statefile (atomic
  write + rename, ~1s heartbeat, state transitions), and
  `rabbit-rs:probe {startup|ready|alive|prestop}` evaluates it with dumb
  read-and-compare semantics — liveness never inspects broker reachability.
  The `RabbitRsProbeEvaluated` event lets synchronous listeners force a
  verdict, and the Laravel reference carries the K8s manifest guidance.
- `rabbit-rs:work --stop-when-empty` (#185): children run exactly once and are
  never recycled; the supervisor exits with the highest child exit status —
  for CI pipelines and pop-once tooling. Without the flag, behavior is
  unchanged.

### Changed

- `ext-rabbit_rs` moves from `require` to `suggest` (#58): `composer install`
  no longer hard-fails without the native extension; resolving a rabbit-rs
  connection raises a precise runtime error with install instructions instead.
  Pint and PHPStan (level 6) join the quality gates and CI.
- `consumers.wait_timeout` is documented as the transport acquisition
  deadline, not the pop wait (#184): the pop wait is the standard `block_for`
  connection key, honored end-to-end.

### Fixed

- Doctor: the broker probe closure now captures the native config (previously
  every healthy broker reported "Undefined variable $nativeConfig"), and the
  Horizon alignment check reads the real `environments.<env>.supervisor-<name>`
  config shape, names supervisors by their config key, and only counts
  supervisors bound to the checked connection. The contradictory
  "inheritance trap" warning is dropped (#186).
- `rabbit-rs:topology --fix` reports success on the declare step itself;
  consumer-profile readiness downgrades to a warning instead of failing the
  bootstrap scenario, and a successful fix resets pre-fix verify failures
  (#195).
- `size()` and `clear()` force-flush the publish buffer before reading,
  restoring the 0.0.9 read-after-dispatch contract (#194).

## [0.1.5] - 2026-09-07

Feature release: synthesized auto worker profiles for `auto_subscribe`, plus a documentation restructure.

### Added

- Core synthesizes default worker profiles for `__auto__.{queue}` names at
  first pop; the auto-subscribe path no longer depends on config mutation and
  works after pool creation.

### Changed

- Documentation restructured into getting-started tracks (native extension
  and Laravel driver, same four-level progression) followed by one
  `reference.md` per track; the Laravel driver reference now lives under
  `packages/laravel-queue/docs/` and travels with the split package mirror.
  The README hero leads with the benefit, and the at-least-once contract is
  scoped to the confirmed delivery path.

## [0.1.4] - 2026-09-06

Bugfix release: `rabbit-rs:status` cross-process queue counters.

### Fixed

- Laravel: the management-API section of `rabbit-rs:status` always reported
  zeros — the command read top-level `messages_delivered`/`messages_acked`/
  `messages_redelivered` keys, while the management API nests cumulative
  counters under `message_stats` and names them `deliver_get`/`ack`/
  `redeliver`. The command now reads the nested object and additionally
  exposes the current queue depth as `messages_ready`.

## [0.1.3] - 2026-09-06

Feature release: the extension binaries are rebuilt with the hardened TLS
transport, and the Laravel package gains two console commands plus
per-subscription adaptive prefetch.

### Added

- Native: the transport enforces TLS certificate verification and sends the
  SNI `server_name` to the broker.
- Native + Laravel: adaptive prefetch per subscription (#42, #162) — a
  `min`/`target buffer`/`max` policy compiled through the driver's compiler,
  adjusted at runtime by the native pool, and observable via
  `getPrefetchStats()`.
- Laravel: `rabbit-rs:doctor` (#155) — one-shot integration diagnostics per
  connection: config compilation, extension version against the composer
  constraint, broker probe, Horizon supervisor alignment, event listeners,
  and publisher safety summary; exits non-zero when a check fails.
- Laravel: `rabbit-rs:topology` (#84, #165) — preflight topology check for
  CI/deploy pipelines: passive queue probes per subscription, and (when
  `management_url` is set) verification of the dead-letter exchange, its
  binding, and each queue's `x-queue-type`; `--fix` (with `--force` outside
  declare mode) declares the missing topology.

### Fixed

- Laravel: child workers receive their index through the dedicated
  `RABBIT_RS_WORKER_INDEX` environment variable (#163) instead of reusing
  the worker-mode variable.
- PHP: extension stubs regenerate without the PHP embed SAPI (#156), so the
  cargo-php tooling builds on macOS without a special link flag.

### Changed

- Docs: the workspace README leads with the product (#161) and the package
  README documents the connection-first configuration.
- CI: the `tls_integration` suite is excluded from the integration job (#159).

## [0.1.2] - 2026-09-06

Packaging/CI release: no Rust or PHP code changes since 0.1.1 (binaries are
functionally identical apart from the version string). It re-runs the release
pipeline end to end after fixing the issues below.

### Fixed

- Release: the `verify-pie-install` container cells install from Packagist
  metadata instead of a VCS repository sync, write Composer `auth.json`
  explicitly (PIE 1.4.10 ignores `COMPOSER_AUTH_JSON`), install `unzip` in
  Debian-based cells (busybox unzip already covers Alpine), and install
  `libgcc` in musl cells (runtime dependency of the Rust-built artifact).
- Release: the Packagist update trigger uses the renamed repository slug
  `Goopil/php-rabbit-rs` (the old `Goopil/rabbit-rs` URL 404s on the
  update-package API now that the package points at the new repository).
- Release: the ext-rabbit_rs constraint policy check derives the expected
  constraint as `^major.minor` instead of `^major.0` for 0.x releases.

## [0.1.1] - 2026-09-05

### Added

- Native: `Pool::stats()` now reports the publish buffer occupancy via two
  new keys, `publish_buffered` and `publish_buffered_bytes` (Round K soak
  tripwire reads them to catch a re-buffer leak path).
- Native: `Pool::stats()` reports `publication_retries_total` — publications
  re-armed once after their deadline expired during a connection recovery
  suspension (#151).
- Benchmarks: the soak harness (`benchmarks/driver-bench/bin/soak.php`)
  records memory telemetry (RSS, PHP usage/peak, `stats()` snapshots) and
  fails on a warmup-excluded RSS-slope leak (`--leak-mb-per-hour`, default
  20 MB/h), on a non-empty publish buffer after a cycle's flush, and —
  in steady mode (`--kill-every=0`) — no longer requires a reconnection
  that only kill churn can produce.

- Native: safe-mode publishes are pipelined — `Pool::publish` no longer blocks
  on the batch flush barrier; confirmations, mandatory returns, and transport
  failures are recorded in a bounded pending-error queue and surface at the
  next pool operation (`publish`, `flush`, `publishBatch`, `stats`,
  `drainErrors`) or, on a queue, through `drainSettlementErrors()`. Explicit
  `flush()`/`publishBatch()` keep full-deadline synchronous semantics;
  process teardown quiesces in-flight drains within a fixed 500 ms budget.
  Fresh-lab benchmark: safe publish 5 729 → 20 866 ops/s (×3.64), reaching
  blind-mode parity (0.95×) and 2.19× vladimir dispatch.

### Fixed

- Core: a publication whose deadline expired while parked during a connection
  recovery suspension is re-armed exactly once with a fresh deadline before
  failing terminally, replayed with the same `message_id` — the first publish
  after an idle no longer fails with "publish deadline expired" when recovery
  outlasts `confirm_timeout`; the failure mode is measured by
  `publication_retries_total` (#151).
- Laravel: `Horizon\RabbitMqQueue` implements `readyNow()` — Horizon's
  AutoScaler calls it unconditionally on every scaling pass, so any supervisor
  pointed at a rabbit-rs connection crash-looped with `Call to undefined
  method ... readyNow()` (#152).
- Laravel: the `worker` cross-cutting default from `config/rabbit-rs.php` is
  inherited by connections that do not declare their own `worker` key (#152).
- Laravel: exhausted rabbit-rs jobs are now recorded as failed in Horizon —
  Horizon's `MarshalFailedEvent` listener only bridges framework `JobFailed`
  events for the Redis job class, so failed jobs stayed marked completed;
  the Horizon job dispatches Horizon's `JobFailed` itself (#152).
- Benchmarks: the soak's RSS-slope leak fit uses a peak-envelope estimator
  seeded by the warmup peak — the raw fit misread bounded allocator
  sawtooth under kill churn as growth (false positive measured on the
  60-min calibration run archived under `benchmarks/results/round-k-soak/`).

- Native: the publish buffer's message list and byte accounting were updated
  under two separate mutexes; a concurrent `take()` could subtract payload
  bytes not yet credited, panicking with `attempt to subtract with overflow`
  and aborting the process (Coverage CI job). Both now mutate under one
  mutex.

## [0.1.0] - 2026-09-02

### Added

- Laravel: connection-first configuration — every broker, its credentials, routes, and consumer profile now live on a single `queue.connections.*` connection in `config/queue.php` (the SQS/redis idiom); `config/rabbit-rs.php` shrinks to ~50 lines of cross-cutting defaults merged under every rabbit-rs connection (per sub-key for `tls`, `delay`, `dead_letter`). There is **no compatibility shim** for the old shape (pre-1.0 break; see the migration table below).
- Laravel: compilation is lazy, per connection, at `connect()` — a config typo only fails the queue driver's use instead of crashing the whole application at boot, Laravel env strings (`'1'`, `'true'`, `'on'`, `"64"`, …) are cast inside the driver, and any error carries the exact `queue.connections.<name>.<key>` path.
- Laravel: `rabbit-rs:work` fans out across connections — with no flags it consumes every queue defined on every rabbit-rs connection (one supervised `queue:work` child per connection); `--connection=a,b` targets connections explicitly; `--queue=x,y` resolves names **by definition** (the connection's `queue` key or a `subscriptions` alias) with a typed error listing available names; a queue defined on two targeted connections is consumed on both; `--workers` now spawns children per connection.
- Laravel: two queue connections with byte-identical arrays compile to the same fingerprint and share one native pool per process (documented feature).
- Laravel: `octane:reload` forgets the queue manager's resolved connections, so the next request recompiles each connection from the current config — broker/credential rotation via env takes effect without stale brokers.

### Changed

- **Breaking (Laravel):** the old `config/rabbit-rs.php` namespaces (`brokers.*`, `routes.*`, `workers.*`) are gone. Migrate your config:

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

  The env hooks `RABBIT_RS_MAX_ATTEMPTS`, `RABBIT_RS_EXCHANGE`, and `RABBIT_RS_QUEUE` are silently dropped from the package config — those values now live as plain connection keys (`max_attempts`, `exchange`, `queue`); wire them with `env()` directly on the connection in `queue.php` if you need an env hook (see [docs/configuration.md](docs/configuration.md)). The same applies to the former broker env hooks `RABBIT_RS_HOSTS`/`RABBIT_RS_VHOST`/`RABBIT_RS_USERNAME`/`RABBIT_RS_PASSWORD`.
- Laravel: `hosts` strings with empty segments (e.g. `"host1:5672,,host2"`) are now strictly rejected with a typed config error instead of silently skipping the empty segment; at least one host is required.
- Laravel: unknown keys — on the connection or inside `tls`, `delay`, `dead_letter`, `subscriptions` — are rejected with their full config path; strictness is otherwise unchanged (`dead_letter` required with `delivery_limit`, `no_ack` requires `early_ack` + `best_effort`, range checks).

## [0.0.9] - 2026-09-01

### Added

- `Pool::clearEventCallbacks()`: removes every registered event callback (connection-state and backpressure combined) and returns how many were removed, so a connection sharing a native pool can re-register from a clean slate.
- Conservative GC for synthesized TTL delay queues: `topology::delay::sweep_delay_queues()` (Rust core) deletes orphaned `rabbit-rs.delay.*` queues through an admin channel once a rolling deploy completes. It only touches the reserved `rabbit-rs.delay.*` prefix, only queues of destinations it is given, keeps every name the current plan still produces and every queue still draining messages, and re-probes emptiness immediately before each delete (quorum queues reject `if-empty`/`if-unused` deletes).
- `Pool::stats()` now reports `dropped_publications_total`: publications the extension discarded without confirmed delivery (deadline-expired flush retries, un-attempted batches on a closing pool, and unconfirmed leftovers at teardown).
- `Pool::stats()` now reports `duplicates_total`: deliveries the broker flags as redelivered (redelivery flag, `x-delivery-count`/`x-acquired-count` > 1) are counted once at dispatch, so duplicates stay measurable per the at-least-once contract. Poison deliveries settled terminally are not counted (they never reach the caller).
- AMQP `Array`/`Table` delivery headers are now exposed as nested PHP arrays, so dead-letter metadata such as `x-death` is visible to PHP. AMQP `Decimal` header values are dropped with a once-per-process PHP notice (PHP has no decimal scalar).
- Opt-in stderr logging from the native extension: set `RABBIT_RS_LOG` to `info`, `warn` (or `warning`) or `error` to install a severity-threshold stderr sink at startup. Without it the extension stays silent; embedders can install their own sink programmatically (first install wins).

### Changed

- TTL delay-queue names now bind the declaring arguments: `rabbit-rs.delay.{destination}.{args}.{bucket}` where `{args}` fingerprints `x-message-ttl`, `x-expires` and the dead-letter target. Two configurations with different buckets or `queue_expiry_margin` values therefore declare distinct queues instead of fighting over one name — `PRECONDITION_FAILED` (406) storms during rolling deploys are gone. Queues synthesized by the previous naming scheme drain through their own message TTL and dead-letter exchange and are eventually deleted by their `x-expires`; the new sweep accelerates the cleanup.
- `Pool::onConnectionState()` and `Pool::onBackpressure()` now register multiple callbacks instead of replacing the previous one, so connections sharing one native pool (e.g. two Laravel connections with the same fingerprint) each keep their own callbacks.
- Laravel: `RabbitMqQueue` clears existing event callbacks before registering its defaults, so worker/pool reuse no longer accumulates duplicate default callbacks (each event firing once per queue construction). To override the defaults, call `Pool::clearEventCallbacks()` before registering a custom callback.
- Publish key validation no longer depends on the build profile: release builds reject unknown publish fields exactly like debug builds (a `delay_ms` typo can no longer publish immediately).
- Configuration surface is enforced: `publisher.mandatory: false` is rejected with a `publisher.safety` migration pointer (honoring it would confirm unroutable publishes = silent loss), `publisher.confirm_timeout` must be ≥ 1 s, and `heartbeat` is bounded to 1..65535 s.
- Laravel: `rabbit-rs` config validation no longer runs at `boot()` — a typo only fails the queue driver's use instead of the whole application; Laravel env-string booleans (`'1'`, `'true'`, `'on'`, …) are accepted; `octane:reload` re-normalizes the config so env-based credential rotation takes effect.
- Blind (fire-and-forget) publishes reserve their payload bytes against the publisher byte budget: an over-budget stream is rejected with backpressure instead of silently growing process memory (the message-count bound already applied).

### Fixed

- Broker connection loss is detected and recovered from automatically: delivery-stream termination and transport error streams trigger recovery, `recovery_failures_total` counts failed generations, and a routine broker restart no longer stops consumption or bricks publishing until the PHP process restarts.
- Workers rejoin a recovered broker: cached consumers are evicted when their source is replaced or closed instead of stopping consumption after every recovery.
- Poison deliveries settle terminally: `consumer.max_attempts` caps redelivery, and over-cap or unmarshable deliveries are rejected to the dead-letter exchange (or documented ack) instead of being redelivered forever.
- Consumer delivery pressure is bounded under `no_ack` (`pending_incoming`), and pool close no longer loses buffered work.
- DLQ bindings are applied for every subscription sharing a dead-letter queue (previously only the first), so poison messages routed per source are not silently lost.
- Delays are validated against the compiled delay strategy: a TTL-mode delay larger than the largest bucket is refused terminally before any transport operation instead of executing immediately; a delayed-release refusal settles the original delivery terminally instead of hot-looping.
- Publisher wake-up: `delay.mode=auto` probes the delay plugin, and a failed topology declare no longer suspends the publisher forever (errors propagate with generation rollback).
- `size()` and `clear()` flush the publish buffer first, so both report/act on the true broker state — previously the first ≤63 publications of a fresh pool stayed in process memory: `size()` returned 0 for accepted messages and `clear()` purged a queue the buffered publications later repopulated.
- The publish buffer arms its flush deadline on the first publication of a batch, so small batches are time-flushed by age at the next trigger instead of waiting for the size threshold.
- `publishBatch()` honors its documented error contract: publications already accepted by an actor are resolved before the first terminal failure is returned, instead of being discarded when a later broker's acquisition fails mid-batch.
- Topology declaration happens before consumer subscription on the recovery and on-demand establishment paths, so a fresh quorum queue can no longer reject `basic.consume` with a 404 and burn a recovery generation.
- Admin operations (`size`, `clear`) run through the connection actor on the per-vhost connection, so they participate in recovery instead of caching a raw connection forever.
- Exceptions thrown inside `onConnectionState`/`onBackpressure` callbacks are no longer silently destroyed: the original exception object is rethrown once the event drain finishes (when the enclosing operation itself fails, the callback exception is preserved in the `$previous` chain of the surfaced error).
- Consumer `next()`, `tryNext()`, and `nextBatch()` drain native events on every call — previously the drain only ran when the delivery buffer was empty, so state/backpressure callbacks starved under steady traffic and dashboards showed healthy state during incidents.
- `Consumer::ackBatch()` now enforces the 256-delivery cap before enqueueing any settlement, so a rejected call has no side effects instead of settling 256 deliveries and then throwing.
- The pool destructor flushes buffered publications under a fixed 500 ms wall-clock budget instead of blocking for up to the per-message timeout (30 s default, 24 h ceiling) at FPM/request shutdown; unconfirmed leftovers are counted in `dropped_publications_total`. Explicit `flush()`/`close()` keep full-deadline semantics.

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

[Unreleased]: https://github.com/Goopil/rabbit-rs/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/Goopil/rabbit-rs/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/Goopil/rabbit-rs/compare/v0.1.4...v0.1.5
[0.1.0]: https://github.com/Goopil/rabbit-rs/compare/v0.0.9...v0.1.0
[0.0.9]: https://github.com/Goopil/rabbit-rs/compare/v0.0.8...v0.0.9
[0.0.8]: https://github.com/Goopil/rabbit-rs/compare/v0.0.7...v0.0.8
[0.0.7]: https://github.com/Goopil/rabbit-rs/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/Goopil/rabbit-rs/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/Goopil/rabbit-rs/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/Goopil/rabbit-rs/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/Goopil/rabbit-rs/compare/v0.0.2...v0.0.3
