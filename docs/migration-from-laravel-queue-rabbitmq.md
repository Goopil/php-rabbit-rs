# Migrating from laravel-queue-rabbitmq

This guide is for applications running
[vladimir-yuldashev/laravel-queue-rabbitmq](https://github.com/vladimir-yuldashev/laravel-queue-rabbitmq)
and moving to [`goopil/rabbit-rs-laravel`](https://github.com/Goopil/php-rabbit-rs/tree/main/packages/laravel-queue).
The Laravel Queue API you call stays the same — `Job::dispatch()`, `queue:work`,
failed-job handling — what changes is the config shape, the transport (a native Rust
extension instead of php-amqplib), and a handful of semantics.

Key names on the upstream side are taken from its `master` readme (v13+ line);
verify against your installed version. Every rabbit-rs key below is validated by the
package's `ConnectionCompiler`, which rejects unknown keys with their full config
path — a typo fails loudly at connection resolution instead of being ignored.

## 1. Install

```bash
pie install goopil/rabbit-rs-native          # native extension
composer require goopil/rabbit-rs-laravel    # queue driver (service provider auto-discovered)
composer remove vladimir-yuldashev/laravel-queue-rabbitmq
```

Requirements: PHP 8.4/8.5, Laravel 12/13, RabbitMQ 4.2.9+.

## 2. Config mapping

The upstream package nests everything under `hosts` (a list of per-host arrays) and
`options.queue`. Rabbit RS is connection-first: one connection in `queue.connections.*`
= one broker/vhost = one native pool, and broker settings sit directly on the
connection.

```php
// config/queue.php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    ],
],
```

Key-by-key mapping (upstream key → rabbit-rs key):

| Upstream (`laravel-queue-rabbitmq`) | Rabbit RS (`queue.connections.<name>`) | Notes |
| ----------------------------------- | -------------------------------------- | ----- |
| `'driver' => 'rabbitmq'` | `'driver' => 'rabbit-rs'` | |
| `'hosts' => [['host' =>, 'port' =>, 'user' =>, 'password' =>, 'vhost' =>]]` | `'hosts' => 'rabbit-1:5672,rabbit-2:5672'` + flat `'vhost'`, `'username'`, `'password'` | one connection per broker/vhost; a bare host defaults to port 5672; IPv6 bracketed (`[::1]:5672`) |
| `'queue' => env('RABBITMQ_QUEUE', 'default')` | `'queue' => env('RABBIT_RS_QUEUE', 'default')` | same role: the default queue of the connection |
| `'options.queue.prioritize_delayed'` | no equivalent | delayed jobs wait in bucket queues (or the delayed-message plugin); there is no prioritized-delay feature |
| `'options.queue.queue_max_priority'` | no equivalent | no `x-max-priority` support |
| `'options.queue.exchange' => 'application-x'` | `'exchange' => 'application-x'` | default `laravel.jobs`; `null` publishes through the default exchange |
| `'options.queue.exchange_type' => 'topic'` | no equivalent | the publish exchange is declared `direct` and durable; for another type, pre-create the exchange on the broker and set `'topology_mode'` to `verify` or `external` |
| `'options.queue.exchange_routing_key' => 'app.%s'` | `'routing_key' => 'app.{queue}'` | `{queue}` is substituted with the queue name (upstream used `%s`); `null` = no routing key |
| `'options.queue.reroute_failed' => true` | no direct equivalent | broker-side `'dead_letter'` + `'delivery_limit'` cover poison messages; Laravel's `failed_jobs` table keeps working unchanged |
| `'options.queue.failed_exchange' => 'failed-exchange'` | `'dead_letter' => ['exchange' => ..., 'queue' => ..., 'routing_key' => ...]` | different mechanism: broker-side dead-lettering (delivery-limit exhaustion, reject without requeue), not app-level failure rerouting |
| `'options.queue.failed_routing_key' => 'app.%s.failed'` | `'dead_letter'['routing_key']` | `null` keeps the original routing key; no `%s` substitution |
| `'options.queue.job' => CustomJob::class` | no equivalent | the payload schema is the standard Laravel one; `RabbitMqJob` exposes `getJobId()` (the stable `message_id`), `attempts()`, `delete()`, `release()` |
| `'connection' => CustomConnection::class` | no equivalent | the transport is the native extension, not php-amqplib — custom connection classes do not apply |
| `'worker' => 'default'` / class name | `'worker' => 'default'` | only `default` or `horizon`; custom worker classes are not supported |
| `'worker' => env('RABBITMQ_WORKER', 'default')` | `'worker' => env('RABBIT_RS_WORKER', 'default')` | Horizon mode, see below |
| `'options' => ['heartbeat' => 10]` | `'heartbeat' => 10` | seconds; default 30 (upstream default was 0) |
| `'secure' => true` + `'options.ssl_options.cafile'` | `'tls' => ['enabled' => true, 'ca_cert' => ...]` | |
| `'options.ssl_options.local_cert'` | `'tls'['client_cert']` | |
| `'options.ssl_options.local_key'` | `'tls'['client_key']` | |
| `'options.ssl_options.verify_peer'` / `'passphrase'` | no equivalent key | TLS accepts exactly `enabled`, `ca_cert`, `client_cert`, `client_key`; anything else is rejected — verify against your version for peer-verification defaults |
| `'after_commit' => true` | `'after_commit' => true` | unchanged (framework key) |
| `'lazy' => false` | no key | pools are always created lazily at first use |
| `'network_protocol' => 'tcp'` | no equivalent | TLS is `'tls'['enabled']` |
| `'options.connection_timeout' / 'read_timeout' / 'write_timeout' / 'channel_rpc_timeout'` | no equivalent | nearest knobs: `'heartbeat'`, `'confirm_timeout'` (ms), `'wait_timeout'` (ms) |

Rabbit RS keys with no upstream counterpart you will likely want:

| Key | Default | Purpose |
| --- | ------- | ------- |
| `'safety'` | `safe` | `safe` (confirms + mandatory) / `unsafe` (no confirms) / `blind` (fire-and-forget) |
| `'confirm_timeout'` | `30000` | publisher confirm timeout (ms) |
| `'prefetch'` | `1000` | QoS prefetch; also `['mode' => 'adaptive', ...]` |
| `'wait_timeout'` | `30000` | broker-connection acquisition deadline (ms), not the `pop()` wait |
| `'topology_mode'` | `declare` | `declare` / `verify` / `external` |
| `'queue_type'` / `'queue_durable'` | `quorum` / `true` | quorum queues by default |
| `'delivery_limit'` | `null` | quorum-queue delivery limit; requires `dead_letter` when set |
| `'dead_letter'` | `null` | `['exchange', 'queue', 'routing_key']` |
| `'delay'` | package defaults | `'mode' => 'auto' \| 'plugin' \| 'ttl'`, buckets, expiry margin |
| `'subscriptions'` | — | multi-queue escape hatch: `['alias' => ['queue' => ..., 'weight' => ..., 'prefetch' => ...]]` |
| `'block_for'` | `null` | seconds `pop()` blocks for work (0 = non-blocking) |
| `'management_url'` | `null` | Laravel-only: management API base URL for `rabbit-rs:status`/`doctor` |

## 3. Worker commands

Upstream offered two consuming paths: `queue:work` (Laravel built-in, `basic.get`,
multi-queue) and the package's own `rabbitmq:consume` (`basic_consume`, single queue,
roughly 2x faster). Rabbit RS collapses this: the native extension always consumes
through `basic_consume`, so there is one built-in command and one supervisor.

| Upstream | Rabbit RS |
| -------- | --------- |
| `php artisan queue:work rabbitmq` | `php artisan queue:work rabbit-rs` |
| `php artisan rabbitmq:consume` | no equivalent needed — `queue:work rabbit-rs` consumes natively; `rabbit-rs:work` adds supervision |
| multiple queues via `queue:work` | one connection consumes several queues via the `'subscriptions'` key, or `rabbit-rs:work --queue=orders,invoices` |
| — | `php artisan rabbit-rs:work` — supervised fan-out: one child per rabbit-rs connection, crash restart with backoff, graceful `SIGTERM`/`SIGINT` |

`rabbit-rs:work` flags (main ones): `--connection=`, `--queue=`, `--workers=1`,
`--max-restarts=3`, `--backoff=1`, `--timeout=60`, `--tries=`, `--memory=128`,
`--max-jobs=`, `--max-time=`, `--stop-when-empty`, `--once`, plus auto-scaling
(`--min-workers`, `--max-workers`).

Operational commands with no upstream counterpart: `rabbit-rs:status` (pool metrics,
`--format=json`), `rabbit-rs:doctor` (health checks, CI-friendly exit codes),
`rabbit-rs:topology` (preflight check, `--fix` declares missing topology),
`rabbit-rs:probe` (Kubernetes probes).

Octane: supported automatically — consumers are closed after each request, pools are
flushed and re-normalized on worker reload. Upstream required warming the connection
in the Octane config; nothing to wire here.

## 4. Horizon compatibility

Rabbit RS supports Laravel Horizon with the same model upstream used since 8.0:
`RABBIT_RS_WORKER=horizon` (or `'worker' => 'horizon'` on the connection), then a
supervisor in `config/horizon.php` pointing at the rabbit-rs connection, configured
exactly like a Redis one. RabbitMQ remains the transport; Redis stores Horizon's
tracking state, metrics, and dashboard data. Redis and Rabbit RS connections can run
in the same Horizon instance.

Without Horizon, keep `RABBIT_RS_WORKER=default` (the default).

## 5. Semantic differences

- **At-least-once delivery.** Duplicates are permitted and identifiable: the job id
  is the stable `message_id`, and a worker crash between receive and acknowledgement
  redelivers the job. Jobs must be idempotent; `duplicates_total` in the pool metrics
  measures the redelivery rate.
- **Publisher confirms on by default.** `safety = safe` derives confirms + mandatory
  routing — no opt-in wiring as with raw php-amqplib, and no separate `mandatory`
  switch. Set `'safety'` explicitly if you depend on unconfirmed publishing.
- **Quorum queues by default.** `queue_type = quorum` (upstream created classic
  queues unless you configured arguments). A quorum queue gives you broker-side
  `delivery_limit` and dead-lettering; the recommended production setup is
  `'delivery_limit' => N` + `'dead_letter'` so poison messages route to a DLQ instead
  of looping. In production, a connection with neither key logs a warning
  (`production_warning => false` silences it).
- **Delayed jobs.** `->delay()` works, but the mechanism is bucket queues or the
  delayed-message plugin (`'delay'['mode'] => auto|plugin|ttl`), not per-message
  priorities — `prioritize_delayed`/`queue_max_priority` have no counterpart.
- **Failed jobs.** Laravel's failed-job pipeline (database table, failing events,
  `--tries`) works unchanged. The broker-level `dead_letter` is an additional layer
  for messages that never reach a job (worker crash before settlement, unmarshable
  payloads, delivery-limit exhaustion).

## 15-minute switch checklist

1. **Install (2 min)** — `pie install goopil/rabbit-rs-native`, `php --ri rabbit_rs`,
   `composer require goopil/rabbit-rs-laravel`, remove the upstream package.
2. **Convert the connection (3 min)** — flatten upstream `hosts[]`/`options.queue`
   into the connection-first keys above; keep one connection per broker/vhost; set
   `QUEUE_CONNECTION` (or your per-queue `onConnection()`) to the new name.
3. **Map options (3 min)** — exchange/routing key/TLS per the table; note the
   `%s` → `{queue}` substitution; decide `delivery_limit` + `dead_letter` (recommended
   in production); drop keys with no equivalent.
4. **Point workers (2 min)** — replace `queue:work rabbitmq` /
   `rabbitmq:consume` in Supervisor/systemd/Horizon config with
   `rabbit-rs:work` (or `queue:work rabbit-rs`); set `RABBIT_RS_WORKER=horizon` if
   you use Horizon.
5. **Smoke-test (3 min)** — dispatch one job, consume it, run
   `php artisan rabbit-rs:doctor` and `rabbit-rs:status`: confirms tracked, no
   returns, queue depth draining.
6. **Remove leftovers (2 min)** — delete the old `rabbitmq` connection block, the
   upstream `options` entries, and any `RABBITMQ_*` env vars; re-run your Pest suite
   without the extension loaded (Unit/Feature) and with it (Integration).
