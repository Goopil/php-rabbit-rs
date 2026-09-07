# Rabbit RS Laravel Queue Driver

A RabbitMQ queue driver for Laravel, built for **high-throughput, long-running workers** and powered by the [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) native PHP extension in Rust. Workload-scoped benchmark results: see the [benchmark harness](https://github.com/Goopil/php-rabbit-rs/blob/main/benchmarks/README.md).

## Why?

The standard Laravel RabbitMQ drivers run in userspace PHP. Rabbit RS moves the connection pool, publisher confirms, consumer scheduling, and connection recovery into a native Rust extension — fewer context switches, bounded memory, and at-least-once delivery without silent loss.

## Features

- **Native connection pooling** — one AMQP connection per vhost, publisher channels pooled, consumer channels dedicated
- **Publisher confirms & mandatory returns** — in `safe` mode every publish is tracked to ACK, return, or timeout; a mandatory return takes precedence over its following ACK
- **At-least-once delivery** — unconfirmed publishes survive connection recovery in bounded process memory and are replayed with the same `message_id` and original deadline
- **Connection-generation-aware ACKs** — stale ACKs are rejected so RabbitMQ redelivers
- **Deterministic recovery** — connection, channels, exchanges, queues, bindings, QoS, then consumers
- **Weighted-fair scheduler** — multiple subscriptions per worker with configurable weights, priority classes, and starvation protection
- **Backpressure events** — `BackpressureDetected` fires during publish and consume operations when the publisher's bounded buffer is full
- **Octane support** — consumers are flushed per-request and pools reloaded on worker restart
- **Quorum queues by default** — durable, delivery-limit-aware topology out of the box
- **Laravel Horizon support** — Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs; coexists with existing Redis queues (see [Laravel Horizon](docs/reference.md#laravel-horizon))

## Requirements

- PHP **8.4** or **8.5**
- Laravel **12** or **13**
- `ext-rabbit_rs` — the native extension (see [Installation](#installation))
- `laravel/horizon` — **optional**, only needed for Horizon dashboard integration (see [Laravel Horizon](docs/reference.md#laravel-horizon))

## Installation

```bash
# Native extension (Linux via PIE)
pie install goopil/rabbit-rs-native

# Laravel queue driver (service provider auto-discovered)
composer require goopil/rabbit-rs-laravel

# Cross-cutting defaults (optional)
php artisan vendor:publish --tag="rabbit-rs-config"
```

Full guide — macOS (Homebrew or manual binary), Docker, multi-PHP, upgrades and rollback: [docs/reference.md — Installation](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#installation).

## Quick Start

The full walkthrough — reliability, scaling, and operations levels — is in [docs/getting-started.md](docs/getting-started.md).

### Queue connection

Add the connection to `config/queue.php`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
    ],
],
```

Set `QUEUE_CONNECTION=rabbit-rs` in your `.env`.

### Dispatch and consume

```php
// Dispatch — standard Laravel API, no changes
ProcessPayment::dispatch($invoice);

// Delayed jobs
SendReport::dispatch($report)->delay(now()->addMinutes(5));
```

```bash
# Run the worker
php artisan rabbit-rs:work

# Supervised — 4 child workers per connection with automatic restart
php artisan rabbit-rs:work --workers=4 --max-restarts=3
```

## Configuration

Rabbit RS is configured **connection-first**: every broker, its credentials, its routes, and its worker profile live on a single **queue connection** in `config/queue.php`, exactly like Laravel's built-in `redis` and `sqs` drivers. One connection = one broker/vhost = one native pool.

| File | Role |
| ---- | ---- |
| `config/queue.php` → `queue.connections.*` | **The primary surface** — broker, credentials, publication, consumption, topology |
| `config/rabbit-rs.php` | Cross-cutting **defaults** merged under every rabbit-rs connection |

Each connection is compiled lazily, when the queue manager first resolves it. Unknown keys and type errors throw `InvalidArgumentException` with the exact config path — e.g. `queue.connections.orders.prefetch` — so a typo only fails the driver's use, never the whole application.

The full reference — every connection key, multiple brokers, the `subscriptions` escape hatch with adaptive prefetch, safety modes, delay modes, topology, validation rules, and pool-reuse semantics — lives in [docs/reference.md](docs/reference.md#configuration).

### Environment variables

The published `config/rabbit-rs.php` wires cross-cutting defaults; connection-only keys (`hosts`, `vhost`, `username`, `password`, `exchange`, `routing_key`, …) are wired directly in `queue.php`:

| Variable | Default | Description |
| -------- | ------- | ----------- |
| `RABBIT_RS_QUEUE` | `default` | Default queue name (wired on the connection) |
| `RABBIT_RS_HOSTS` | `127.0.0.1:5672` | Comma-separated broker addresses (wired on the connection) |
| `RABBIT_RS_VHOST` | `/` | AMQP virtual host (wired on the connection) |
| `RABBIT_RS_USERNAME` | `guest` | Broker username (wired on the connection) |
| `RABBIT_RS_PASSWORD` | `guest` | Broker password (wired on the connection) |
| `RABBIT_RS_EXCHANGE` | `laravel.jobs` | Publishing exchange (wired on the connection) |
| `RABBIT_RS_HEARTBEAT` | `30` | AMQP heartbeat in seconds |
| `RABBIT_RS_TLS` | `false` | Enable TLS |
| `RABBIT_RS_TLS_CA_CERT` | — | Path to CA certificate |
| `RABBIT_RS_TLS_CLIENT_CERT` | — | Path to client certificate (mTLS) |
| `RABBIT_RS_TLS_CLIENT_KEY` | — | Path to client key (mTLS) |
| `RABBIT_RS_SAFETY` | `safe` | `safe`, `unsafe`, or `blind` |
| `RABBIT_RS_CONFIRM_TIMEOUT` | `30000` | Publisher confirm timeout in ms |
| `RABBIT_RS_PREFETCH` | `64` | QoS prefetch per consumer channel |
| `RABBIT_RS_CONSUMER_WAIT_TIMEOUT` | `30000` | Consumer acquisition deadline in ms |
| `RABBIT_RS_TOPOLOGY_MODE` | `declare` | `declare`, `verify`, or `external` |
| `RABBIT_RS_DELAY_MODE` | `auto` | `auto`, `plugin`, or `ttl` |
| `RABBIT_RS_DELAY_BUCKETS` | `1,5,30,120` | Comma-separated delay buckets in seconds |
| `RABBIT_RS_DELAY_MAX_BUCKETS` | `8` | Max TTL bucket queues allowed |
| `RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN` | `60` | Extra `x-expires` margin for bucket queues, in seconds |
| `RABBIT_RS_WORKER` | `default` | Worker mode: `default` or `horizon` || `RABBIT_RS_PRODUCTION_WARNING` | `true` | Warn about unbounded redeliveries without `delivery_limit` + dead-letter |
| `RABBIT_RS_BEST_EFFORT` | `false` | Gates `early_ack`/`no_ack` subscriptions on a connection |

## Commands

| Command | Purpose |
| ------- | ------- |
| `php artisan rabbit-rs:work` | Supervised fan-out worker — one child per connection, crash restart with exponential backoff (`--connection`, `--queue`, `--workers`, `--max-restarts`, `--backoff`) |
| `php artisan rabbit-rs:status` | Per-connection pool metrics and counters; `--format=json` for monitoring |
| `php artisan rabbit-rs:doctor` | One-shot health report per connection — `ok`/`warn`/`fail` checks, non-zero exit on failure (CI-friendly) |
| `php artisan rabbit-rs:topology` | Preflight topology check; `--fix` declares missing topology |

Details — dispatching, worker and queue resolution semantics, status counters, doctor checks, and topology verification: [docs/reference.md](docs/reference.md).

## Recipes

Task-oriented guides in the repository docs:

- [Topology patterns](docs/reference.md#recipe-topology-patterns) — work queue vs pub-sub vs delayed, dead-letter wiring, gating topology in CI with `rabbit-rs:topology` / `rabbit-rs:doctor`
- [Broker tuning](docs/reference.md#recipe-broker-tuning) — queue types, watermarks, max-length, heartbeat/confirm timeout, prefetch
- [Capacity planning](docs/reference.md#recipe-capacity-planning) — sizing from the Round K soak evidence and the benchmark harness

## Laravel Horizon

Rabbit RS integrates with [Laravel Horizon](https://laravel.com/docs/horizon): set `RABBIT_RS_WORKER=horizon` (or `worker => 'horizon'` on the connection) and Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs, with Redis used for observability only. Full setup, supervisor coexistence, and the `JobPending`/`JobPushed`/`JobReserved`/`JobDeleted` event contract: [docs/reference.md — Laravel Horizon](docs/reference.md#laravel-horizon).

## Octane

When Laravel Octane is detected, the driver automatically closes cached consumers after each request, flushes all pools on worker reload (re-normalizing config so broker or credential rotation takes effect), and stops all pools on worker shutdown. No configuration needed — the lifecycle hooks are registered by the service provider. See [docs/reference.md — Octane](docs/reference.md#octane-integration).

## Events

Two native events, drained synchronously on the PHP thread during publish/consume/flush calls (no polling): `BackpressureDetected` (the publisher's bounded buffer is full) and `ConnectionStateChanged` (a broker connection changes state). When `worker=horizon`, Horizon's own lifecycle events are dispatched too. Payloads, listeners, and custom callbacks: [docs/reference.md — Events](docs/reference.md#events).

## License

MIT. See [LICENSE](https://github.com/Goopil/php-rabbit-rs/blob/main/LICENSE).
