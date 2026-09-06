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
- **Laravel Horizon support** — Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs; coexists with existing Redis queues

## Requirements

- PHP **8.4** or **8.5**
- Laravel **12** or **13**
- `ext-rabbit_rs` — the native extension (see [installation](#installation))
- `laravel/horizon` — **optional**, only needed for Horizon dashboard integration (see [Horizon](#laravel-horizon))

## Installation

### 1. Install the native extension

**Linux** — via [PIE](https://github.com/php/pie):

```bash
pie install goopil/rabbit-rs-native
```

**macOS (Apple Silicon)** — via [Homebrew](https://github.com/Goopil/homebrew-rabbit-rs):

```bash
brew tap goopil/rabbit-rs
brew install rabbit-rs
```

**Manual (any platform)** — download the ZIP matching your PHP version, architecture, and libc from the [releases page](https://github.com/Goopil/php-rabbit-rs/releases), then load it:

```bash
# Example: PHP 8.4, Apple Silicon, NTS
unzip php_rabbit_rs-*_php8.4-arm64-darwin-nts.zip
cp rabbit_rs.so $(php-config --extension-dir)/rabbit_rs.so

# Enable it
echo "extension=rabbit_rs" > $(php-config --ini-dir)/ext-rabbit_rs.ini

# Verify
php --ri rabbit_rs
```

### 2. Install the Composer package

```bash
composer require goopil/rabbit-rs-laravel
```

The service provider is auto-discovered by Laravel.

### 3. Publish the config

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

This creates `config/rabbit-rs.php` with inline comments explaining every option.

## Quick Start

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

The full connection reference, subscription escape hatch, validation rules, and pool-reuse semantics live in [docs/configuration.md](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/configuration.md).

### Queue connection

Add one connection to `config/queue.php`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),

        // Broker — one connection = one broker/vhost = one native pool
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'), // comma-separated, IPv6 bracketed: [::1]:5672
        'vhost' => env('RABBIT_RS_VHOST', '/'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'heartbeat' => env('RABBIT_RS_HEARTBEAT', 30),
        'tls' => [
            'enabled' => (bool) env('RABBIT_RS_TLS', false),
            'ca_cert' => env('RABBIT_RS_TLS_CA_CERT'),
            'client_cert' => env('RABBIT_RS_TLS_CLIENT_CERT'),
            'client_key' => env('RABBIT_RS_TLS_CLIENT_KEY'),
        ],

        // Publication
        'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'), // null = default exchange (direct-to-queue)
        'routing_key' => '{queue}',                              // {queue} placeholder; null = no routing key
        'safety' => env('RABBIT_RS_SAFETY', 'safe'),             // safe | unsafe | blind
        'confirm_timeout' => env('RABBIT_RS_CONFIRM_TIMEOUT', 30000), // ms, >= 1000

        // Consumption
        'prefetch' => env('RABBIT_RS_PREFETCH', 64),             // per consumer channel = per worker process
        'wait_timeout' => env('RABBIT_RS_CONSUMER_WAIT_TIMEOUT', 30000), // ms

        // Topology (defaults inherited from config/rabbit-rs.php)
        'topology_mode' => env('RABBIT_RS_TOPOLOGY_MODE', 'declare'), // declare | verify | external
        'queue_type' => 'quorum',       // quorum | classic
        'queue_durable' => true,
        'delivery_limit' => null,       // null = no limit; requires dead_letter when set
        'dead_letter' => null,
    ],
],
```

Every key except `driver` and `queue` is optional — anything the connection omits falls back to `config/rabbit-rs.php` (per sub-key for `tls`, `delay`, and `dead_letter`); keys with no entry there use the driver's built-in defaults. The connection above is therefore equivalent to the minimal form shown in [Quick Start](#quick-start).

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
| `RABBIT_RS_WORKER` | `default` | Worker mode: `default` or `horizon` |
| `RABBIT_RS_AUTO_SUBSCRIBE` | `false` | Let `pop()` resolve plain queue names via implicit profiles (requires native runtime profiles — not yet available; declare queues in `subscriptions`) |
| `RABBIT_RS_PRODUCTION_WARNING` | `true` | Warn about unbounded redeliveries without `delivery_limit` + dead-letter |
| `RABBIT_RS_BEST_EFFORT` | `false` | Gates `early_ack`/`no_ack` subscriptions on a connection |

### Multiple brokers

A vhost owns a distinct AMQP connection. To consume from or publish to several brokers or vhosts, define one connection per broker — the framework's own way of expressing multiple backends:

```php
'connections' => [
    'orders-eu' => [
        'driver' => 'rabbit-rs',
        'queue' => 'orders',
        'hosts' => ['rabbit-1:5672', 'rabbit-2:5672'],
        'vhost' => '/orders-eu',
        'username' => 'orders',
        'password' => env('ORDERS_PASSWORD'),
        'exchange' => 'laravel.jobs',
    ],

    'billing' => [
        'driver' => 'rabbit-rs',
        'queue' => 'invoices',
        'hosts' => 'rabbit-3:5672',
        'vhost' => '/billing',
        'username' => 'billing',
        'password' => env('BILLING_PASSWORD'),
        'tls' => [
            'enabled' => true,
            'ca_cert' => '/etc/ssl/certs/rabbit-ca.pem',
        ],
        'exchange' => 'billing.jobs',
    ],
],
```

```php
BillingJob::dispatch($invoice)->onConnection('billing');
OrdersJob::dispatch($orderId)->onConnection('orders-eu');
```

```bash
# Consume every queue of every rabbit-rs connection
php artisan rabbit-rs:work

# Consume only the listed connections
php artisan rabbit-rs:work --connection=orders-eu,billing
```

**`hosts`** — Comma-separated list of `host:port` endpoints (or an array of such strings). Endpoints are sorted and the pool connects to the first reachable host, failing over on recovery. IPv6 addresses must be bracketed: `[::1]:5672`.

### Subscriptions

By default a connection consumes exactly one queue: its `queue` key. Set `subscriptions` to consume several queues on the same broker with per-subscription tuning. One composed consumer fans deliveries in through a single `pop()` call under weighted-fair scheduling:

```php
'orders-eu' => [
    // ...
    'subscriptions' => [
        'critical' => [
            'queue' => 'orders.critical',
            'weight' => 8,
            'priority_class' => 1,
            'prefetch' => 8,
        ],
        'bulk' => [
            'queue' => 'orders.bulk',
            'weight' => 2,
            'prefetch' => 32,
            'starvation_after' => 60,
        ],
    ],
],
```

**`weight`** (default `1`) — Delivery share vs other subscriptions (1–65535). Higher weight gets more consumer credit.

**`priority_class`** (default `0`) — Inter-queue priority (-32768 to 32767); higher numbers are served first. This is client-side scheduler state — nothing is sent to the broker.

**`prefetch`** (default: the connection's `prefetch`) — QoS prefetch for this subscription's dedicated channel.

**`starvation_after`** (default `30`) — Seconds before aging kicks in: the subscription's effective priority is raised to prevent starvation by higher-priority subscriptions.

Rules: at least one entry, unique queues across aliases, unknown fields rejected, and a subscription cannot cross brokers (its broker is always its own connection — use a second connection for that). `early_ack`/`no_ack` tuning exists behind the `best_effort` gate; see [docs/configuration.md](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/configuration.md).

### Safety modes

The `safety` setting selects the safety mode; publisher confirms and mandatory
routing are **derived from it**, never set independently:

| Mode | Behaviour |
| ---- | --------- |
| `safe` (default) | Confirms + mandatory routing. At-least-once: unconfirmed publications are retained in bounded process memory and replayed with their original `message_id` across connection recovery. |
| `unsafe` | Confirms without mandatory routing — unroutable messages are silently dropped by the broker. |
| `blind` | Explicit fire-and-forget through a bounded background pump. A transport failure after the hand-off is a silent loss; delayed jobs are **not** honored. |

**`confirm_timeout`** — Milliseconds to wait for a confirm before treating the publish as failed. During a recovery, a publish parked in replay is retried once with a fresh deadline; a confirm timeout on a live connection stays terminal (unknown outcome → no automatic resend).

### Delayed Messages

Controls how delayed jobs (`Job::dispatch()->delay(...)`) are handled. Per-connection keys: `mode`, `buckets`, `max_buckets`, `queue_expiry_margin`.

| Mode | Behaviour |
| ---- | -------- |
| `auto` | Publish delayed messages through the `rabbitmq_delayed_message_exchange` plugin (same as `plugin`). Use `ttl` when the plugin is not installed. |
| `plugin` | Always use the delayed exchange plugin. Fails if the plugin is not installed. |
| `ttl` | Always use bucketed TTL queues. Creates one queue per bucket with a per-message TTL and dead-lettering to the target queue. |

**`buckets`** — Delay thresholds in seconds. Messages are placed in the bucket whose TTL is the smallest value ≥ the requested delay.

**`queue_expiry_margin`** — Extra TTL (seconds) added to bucket queues (`x-expires`) so they survive brief broker restarts.

### Topology

Defines the queue and dead-letter topology the driver declares when `topology_mode` is `declare`. Flat keys on the connection:

```php
'queue_type' => 'quorum',       // quorum (default) or classic
'queue_durable' => true,
'delivery_limit' => null,       // null = no limit; requires dead_letter when set
'dead_letter' => null,
```

**`queue_type`** — `quorum` (default) for replicated, Raft-based queues with delivery limits. Recommended for production. `classic` for single-node durable queues.

**`delivery_limit`** — Max redelivery count on quorum queues. After this many attempts, the message is dead-lettered. **`dead_letter` MUST be configured when `delivery_limit` is set** — without a DLX, poison messages are silently dropped after the limit is reached. Set to `null` to disable the delivery limit entirely.

#### Dead-letter exchange

```php
'queue_type' => 'quorum',
'delivery_limit' => 20,
'dead_letter' => [
    'exchange' => 'dead-letters',
    'queue' => 'failed-jobs',
    'routing_key' => null,  // null = use original routing key
],
```

When set, messages that exceed `delivery_limit` are routed here instead of being silently dropped.

### Topology Mode

| Mode | Behaviour |
| ---- | -------- |
| `declare` | Create exchanges, queues, and bindings if they don't exist. DDL is idempotent and matches the config. |
| `verify` | Check that the declared topology exists but never create. Fails fast if missing. |
| `external` | Don't touch topology at all. The broker is expected to be fully configured externally. |

## Usage

### Dispatching jobs

```php
// Standard Laravel dispatch — no API changes
ProcessPayment::dispatch($invoice);

// Delayed
SendReport::dispatch($report)->delay(now()->addMinutes(5));

// On a specific queue
ProcessPayment::dispatch($invoice)->onQueue('high-priority');
```

### Running the worker

```bash
# Every rabbit-rs connection, every defined queue
php artisan rabbit-rs:work

# Restrict to connections and queues (comma-separated; queue names are
# resolved by definition: the connection's `queue` key or a subscriptions alias)
php artisan rabbit-rs:work --connection=orders-eu,billing
php artisan rabbit-rs:work --queue=critical

# 4 child workers per connection
php artisan rabbit-rs:work --connection=orders-eu --workers=4
```

The supervisor spawns one `queue:work` child **per targeted connection** — each child consumes that connection's whole queue set through the native weighted-fair scheduler — and restarts children on exit: a clean exit (0, e.g. `--max-jobs` recycling) restarts immediately without consuming the restart budget, while a non-zero exit is treated as a crash and restarted with exponential backoff (capped at 60 s) up to `--max-restarts` times.

| Option | Description | Default |
| ------ | ----------- | ------- |
| `--connection` | Comma-separated queue connections | every rabbit-rs connection |
| `--queue` | Comma-separated queue names, resolved by definition | every defined queue |
| `--workers` | Child workers per connection | `1` |
| `--max-restarts` | Max crash restarts per child worker | `3` |
| `--backoff` | Base backoff in seconds | `1` |
| `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` | Propagated to each `queue:work` child | `60`, —, `128`, —, — |

### Status diagnostics

```bash
# Human-readable
php artisan rabbit-rs:status

# JSON for monitoring
php artisan rabbit-rs:status --format=json
```

Prints per-connection native pool metrics: handle state (PID, closed), cumulative publisher counters (publishes, confirmations, returns, backpressure, reconnects, duplicates), consumer counters (deliveries, acks, rejects), and confirmation/settlement latency percentiles. These metrics are same-process only.

When a connection defines `management_url`, queue counters (`delivered`, `acked`, `redelivered`) are also fetched from the RabbitMQ management API for a cross-process view — `redelivered` is an approximate duplicate signal (it also counts crash requeues).

### Integration diagnostics

```bash
php artisan rabbit-rs:doctor

# Single connection
php artisan rabbit-rs:doctor --connection=rabbit-rs
```

One-shot health report per rabbit-rs connection, resolved through the same config compilation the driver uses. Each check prints `ok`, `warn`, or `fail`; the command exits non-zero when any check fails (warnings are allowed), which makes it usable in CI.

Checks: extension presence and version against the `ext-rabbit_rs` composer constraint, resolved worker class (with a loud warning when `worker` is inherited from the package defaults instead of the connection — the known `queue.connections.<name>.worker` trap), broker reachability (AMQP connect, auth, vhost; optional management API probe when `management_url` is set; skipped with a warning when the extension is not loaded), publisher exchange/routing-key alignment and dead-letter wiring, effective safety settings after compilation, Horizon supervisors against the connection subscriptions, and which package events (`BackpressureDetected`, `ConnectionStateChanged`) have listeners.

### Octane

When Laravel Octane is detected, the driver automatically:

- Closes cached consumers after each request (prevents channel leaks)
- Flushes all pools on worker reload
- Re-normalizes the `rabbit-rs` config on worker reload — broker or credential rotation via env variables takes effect for connections resolved after the reload
- Stops all pools on worker shutdown

No configuration needed — the lifecycle hooks are registered by the service provider.

## Laravel Horizon

Rabbit RS integrates with [Laravel Horizon](https://laravel.com/docs/horizon) so jobs processed via RabbitMQ appear in the Horizon dashboard alongside Redis jobs. RabbitMQ remains the transport; Redis is used by Horizon for job tracking, metrics, and dashboard state.

### Setup

1. Install Horizon:

```bash
composer require laravel/horizon
php artisan horizon:install
```

2. Set the worker mode to `horizon`:

```bash
# .env
RABBIT_RS_WORKER=horizon
```

Or per-connection in `config/queue.php`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'horizon',
    ],
],
```

3. Configure Horizon supervisors for both Redis and Rabbit RS in `config/horizon.php`:

```php
'environments' => [
    'production' => [
        'supervisor-redis' => [
            'connection' => 'redis',
            'queue' => ['default', 'notifications'],
            'maxProcesses' => 5,
        ],
        'supervisor-rabbit' => [
            'connection' => 'rabbit-rs',
            'queue' => ['orders', 'billing'],
            'maxProcesses' => 3,
        ],
    ],
],
```

### How it works

When `worker=horizon`, the driver uses `Horizon\RabbitMqQueue` which dispatches Horizon events at each job lifecycle stage:

| Stage | Event | When |
|-------|-------|------|
| Before push | `JobPending` | `push()` before sending to RabbitMQ |
| After push | `JobPushed` | `push()` after the broker accepts the message |
| After pop | `JobReserved` | `pop()` returns a job to the worker |
| After delete | `JobDeleted` | `delete()` acknowledges the job |

Horizon stores this metadata in Redis and displays it in the dashboard. The RabbitMQ message itself is the source of truth for delivery — Horizon's Redis state is for observability only.

### Coexistence with Redis queues

Both Redis and Rabbit RS connections can run in the same Horizon instance. Redis queues use Horizon's native Redis backend; Rabbit RS queues dispatch events that Horizon tracks in Redis while using RabbitMQ as the actual transport.

### Without Horizon

If Horizon is not installed, set `RABBIT_RS_WORKER=default` (the default). The `horizon` mode requires `laravel/horizon` to be installed.

## Events

Native events are drained synchronously on the PHP thread during `publish()`, `publishBatch()`, `flush()`, consumer `next()`/`tryNext()`/`nextBatch()`, and `stats()` calls — no polling required.

| Event | Fired when | Payload |
| ----- | ---------- | ------- |
| `BackpressureDetected` | The publisher's bounded buffer is full | `broker`, `inFlight`, `capacity` |
| `ConnectionStateChanged` | A broker connection changes state | `broker`, `state`, `generation` |

When `worker=horizon`, Horizon's own events (`JobPending`, `JobPushed`, `JobReserved`, `JobDeleted`) are also dispatched. See [Laravel Horizon](#laravel-horizon).

Listen in your `EventServiceProvider`:

```php
protected $listen = [
    \Goopil\RabbitRs\Laravel\Events\BackpressureDetected::class => [
        \App\Listeners\AlertOnBackpressure::class,
    ],
    \Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged::class => [
        \App\Listeners\LogConnectionState::class,
    ],
];
```

## Testing

```bash
# Unit + Feature tests (no broker required)
php vendor/bin/pest tests/Unit tests/Feature

# Integration tests (requires a running RabbitMQ broker)
php vendor/bin/pest tests/Integration
```

## Architecture

```
┌─────────────────────────────────────────────┐
│  Laravel Application                        │
│  Job::dispatch() → Queue::push()             │
├─────────────────────────────────────────────┤
│  RabbitMqQueue (PHP driver layer)           │
│  Horizon\RabbitMqQueue (when worker=horizon) │
│  MessageMapper · WorkerProfileResolver      │
├─────────────────────────────────────────────┤
│  NativePoolFactory → Pool (Rust)            │
│  Connection pool · Publisher confirms       │
│  Consumer scheduler · Recovery · Backpress │
├─────────────────────────────────────────────┤
│  RabbitMQ broker                            │
└─────────────────────────────────────────────┘
```

The PHP layer maps Laravel jobs to native messages and delegates all I/O to the Rust extension. The native pool manages connection recovery, publisher confirms, and consumer scheduling — none of this runs in PHP userspace.

## License

MIT. See [LICENSE](https://github.com/Goopil/php-rabbit-rs/blob/main/LICENSE).
