# Rabbit RS Laravel Queue Driver

A high-performance RabbitMQ queue driver for Laravel, powered by the [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) native PHP extension built in Rust.

## Why?

The standard Laravel RabbitMQ drivers run in userspace PHP. Rabbit RS moves the connection pool, publisher confirms, consumer scheduling, and connection recovery into a native Rust extension — fewer context switches, bounded memory, and at-least-once delivery without silent loss.

## Features

- **Native connection pooling** — one AMQP connection per vhost, publisher channels pooled, consumer channels dedicated
- **Publisher confirms & mandatory returns** — every publish is tracked to ACK, return, or timeout; a mandatory return takes precedence over its following ACK
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

The native extension ships as pre-compiled `.so` binaries on the [releases page](https://github.com/Goopil/php-rabbit-rs/releases).

```bash
# Download the .so matching your PHP version, architecture, and libc
# Example: PHP 8.4, x86_64, glibc, NTS
cp php_rabbit_rs-*.so $(php -r "echo ini_get('extension_dir');")

# Enable it
echo "extension=rabbit_rs" > $(php -r "echo PHP_CONFIG_FILE_SCAN_DIR;")/rabbit_rs.ini

# Verify
php -m | grep rabbit_rs
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
php artisan rabbit-rs:work --queue=default

# Supervised — 4 child workers with automatic restart
php artisan rabbit-rs:work --queue=default --workers=4 --max-restarts=3
```

## Configuration

The full configuration lives in `config/rabbit-rs.php`. Every option is documented inline. Below is a structured reference.

### Environment Variables

| Variable | Description | Default |
| -------- | ----------- | ------- |
| `RABBIT_RS_HOSTS` | Comma-separated broker addresses (`host:port,host:port`) | `127.0.0.1:5672` |
| `RABBIT_RS_VHOST` | AMQP virtual host | `/` |
| `RABBIT_RS_USERNAME` | Broker username | `guest` |
| `RABBIT_RS_PASSWORD` | Broker password | `guest` |
| `RABBIT_RS_QUEUE` | Default queue name | `default` |
| `RABBIT_RS_EXCHANGE` | Default exchange for routing | `laravel.jobs` |
| `RABBIT_RS_HEARTBEAT` | AMQP heartbeat in seconds | `30` |
| `RABBIT_RS_PREFETCH` | QoS prefetch count per consumer | `16` |
| `RABBIT_RS_CONFIRM_TIMEOUT` | Publisher confirm timeout in ms | `30000` |
| `RABBIT_RS_TOPOLOGY_MODE` | `declare`, `verify`, or `external` | `declare` |
| `RABBIT_RS_DELAY_MODE` | `auto`, `plugin`, or `ttl` | `auto` |
| `RABBIT_RS_DELAY_BUCKETS` | Comma-separated delay buckets in seconds | `1,5,30,120` |
| `RABBIT_RS_DELAY_MAX_BUCKETS` | Max TTL bucket queues allowed | `8` |
| `RABBIT_RS_TLS` | Enable TLS | `false` |
| `RABBIT_RS_TLS_CA_CERT` | Path to CA certificate | — |
| `RABBIT_RS_TLS_CLIENT_CERT` | Path to client certificate (mTLS) | — |
| `RABBIT_RS_TLS_CLIENT_KEY` | Path to client key (mTLS) | — |
| `RABBIT_RS_TLS_SERVER_NAME` | Expected server name for SNI | — |
| `RABBIT_RS_TLS_VERIFY` | `peer` or `none` | `peer` |
| `RABBIT_RS_WORKER` | Worker mode: `default` or `horizon` | `default` |

### Brokers

Each broker is a named connection pool. A vhost owns a distinct AMQP connection — publisher channels are pooled, consumer channels are dedicated.

```php
'brokers' => [
    // Primary cluster
    'default' => [
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'vhost' => env('RABBIT_RS_VHOST', '/'),
        'credentials' => [
            'username' => env('RABBIT_RS_USERNAME', 'guest'),
            'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        ],
        'heartbeat' => (int) env('RABBIT_RS_HEARTBEAT', 30),
    ],

    // Secondary broker for a different vhost or cluster
    'analytics' => [
        'hosts' => 'rabbitmq-analytics.internal:5672',
        'vhost' => '/analytics',
        'credentials' => [
            'username' => env('RABBIT_RS_ANALYTICS_USER', 'analytics'),
            'password' => env('RABBIT_RS_ANALYTICS_PASS', 'secret'),
        ],
        'heartbeat' => 60,
    ],
],
```

**`hosts`** — Comma-separated list of `host:port` endpoints. The pool connects to the first available and fails over on recovery. IPv6 addresses must be bracketed: `[::1]:5672`.

**`vhost`** — Each unique vhost gets its own connection within the pool.

**`heartbeat`** — If no data is exchanged for 2× this interval, the connection is considered dead and recovery kicks in. Keep below your TCP keepalive threshold.

### TLS

```php
'tls' => [
    'enabled' => (bool) env('RABBIT_RS_TLS', false),
    'ca_cert' => env('RABBIT_RS_TLS_CA_CERT'),
    'client_cert' => env('RABBIT_RS_TLS_CLIENT_CERT'),
    'client_key' => env('RABBIT_RS_TLS_CLIENT_KEY'),
],
```

Set `enabled=true` for `amqps://`. The `ca_cert` enables server certificate verification. `client_cert` and `client_key` enable mutual TLS.

### Routes

Routes map Laravel queue names to AMQP exchanges and routing keys. When you dispatch a job to a queue, the driver looks up the route by name.

```php
'routes' => [
    'default' => [
        'broker' => 'default',
        'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
        'routing_key' => '{queue}',
    ],

    // Route a queue to a different broker
    'analytics' => [
        'broker' => 'analytics',
        'exchange' => 'analytics.events',
        'routing_key' => 'events.{queue}',
    ],
],
```

**`routing_key`** — The placeholder `{queue}` is replaced with the actual queue name at publish time. Use a fixed string for topic-based routing.

### Workers

Each worker profile defines a set of subscriptions consumed by a single `rabbit-rs:work` process. The native scheduler multiplexes subscriptions on a dedicated channel per consumer.

```php
'workers' => [
    'default' => [
        'scheduler' => [
            'strategy' => 'weighted_fair',
        ],
        'subscriptions' => [
            'default' => [
                'enabled' => true,
                'broker' => 'default',
                'queue' => env('RABBIT_RS_QUEUE', 'default'),
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => [
                    'mode' => 'fixed',
                    'value' => (int) env('RABBIT_RS_PREFETCH', 16),
                ],
                'starvation_after' => 30,
            ],
        ],
    ],

    // Worker with multiple subscriptions and priority
    'multi' => [
        'scheduler' => [
            'strategy' => 'weighted_fair',
        ],
        'subscriptions' => [
            'high-priority' => [
                'enabled' => true,
                'broker' => 'default',
                'queue' => 'priority',
                'weight' => 4,
                'priority_class' => -1,
                'prefetch' => ['mode' => 'fixed', 'value' => 32],
                'starvation_after' => 15,
            ],
            'default' => [
                'enabled' => true,
                'broker' => 'default',
                'queue' => 'default',
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => ['mode' => 'fixed', 'value' => 16],
                'starvation_after' => 30,
            ],
            // Disabled subscription — kept in config but skipped
            'legacy' => [
                'enabled' => false,
                'broker' => 'default',
                'queue' => 'legacy-jobs',
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => ['mode' => 'fixed', 'value' => 8],
                'starvation_after' => 30,
            ],
        ],
    ],
],
```

**`strategy`** — Currently only `weighted_fair` is supported. The scheduler distributes consumer credit proportional to weight.

**`weight`** — Relative weight in the weighted-fair scheduler. Higher weight gets more consumer credit.

**`priority_class`** — Integer (-32768 to 32767). Lower numbers are higher priority on quorum queues. The scheduler groups subscriptions by class and serves the highest priority first.

**`prefetch.value`** — QoS prefetch count. The broker delivers at most this many unacked messages per consumer channel.

**`starvation_after`** — Seconds without a delivery before the scheduler boosts this subscription's weight to prevent starvation by heavier-weight subscriptions.

### Publisher

Controls how publishes are confirmed by the broker.

```php
'publisher' => [
    'confirms' => true,
    'mandatory' => true,
    'confirm_timeout' => (int) env('RABBIT_RS_CONFIRM_TIMEOUT', 30000),
],
```

**`confirms`** — Every publish is tracked until the broker ACKs or returns it. Unconfirmed publishes survive connection recovery in bounded process memory and are replayed with the same `message_id` and original deadline.

**`mandatory`** — The broker returns messages that cannot be routed (no queue matches the routing key). A return takes precedence over a following ACK.

**`confirm_timeout`** — Milliseconds to wait for a confirm before treating the publish as failed. A timeout resolves the waiter once — it does not mean the message was lost, only that confirmation didn't arrive in time.

### Delayed Messages

Controls how delayed jobs (`Job::dispatch()->delay(...)`) are handled.

```php
'delay' => [
    'mode' => env('RABBIT_RS_DELAY_MODE', 'auto'),
    'buckets' => array_map('intval', array_filter(array_map('trim', explode(',', env('RABBIT_RS_DELAY_BUCKETS', '1,5,30,120'))))),
    'max_buckets' => (int) env('RABBIT_RS_DELAY_MAX_BUCKETS', 8),
    'queue_expiry_margin' => (int) env('RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN', 60),
    'detection_timeout' => (int) env('RABBIT_RS_DELAY_DETECTION_TIMEOUT', 5),
],
```

**`mode`**

| Mode | Behaviour |
| ---- | -------- |
| `auto` | Detect the `rabbitmq_delayed_message_exchange` plugin. Use it if present, fall back to TTL bucketed queues if not. |
| `plugin` | Always use the delayed exchange plugin. Fails if the plugin is not installed. |
| `ttl` | Always use bucketed TTL queues. Creates one queue per bucket with a per-message TTL and dead-letter to the target queue. |

**`buckets`** — Delay thresholds in seconds. Messages are placed in the bucket whose TTL is the smallest value ≥ the requested delay.

**`queue_expiry_margin`** — Extra TTL (seconds) added to bucket queues so they survive brief broker restarts.

### Topology

Defines the queue and dead-letter topology the driver declares when `topology_mode` is `declare`.

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',       // quorum (default) or classic
        'durable' => true,
        'delivery_limit' => null, // null = no limit; requires dead_letter when set
    ],
    'dead_letter' => null,
],
```

**`queue.type`** — `quorum` (default) for replicated, Raft-based queues with delivery limits. Recommended for production. `classic` for single-node durable queues.

**`queue.delivery_limit`** — Max redelivery count on quorum queues. After this many attempts, the message is dead-lettered. **`dead_letter` MUST be configured when `delivery_limit` is set** — without a DLX, poison messages are silently dropped after the limit is reached. Set to `null` to disable the delivery limit entirely.

#### Dead-letter exchange

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => 20,
    ],
    'dead_letter' => [
        'exchange' => 'dead-letters',
        'queue' => 'failed-jobs',
        'routing_key' => null,  // null = use original routing key
    ],
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
# Single worker
php artisan rabbit-rs:work --queue=default

# Supervised — 4 child workers with automatic restart
php artisan rabbit-rs:work --queue=default --workers=4 --max-restarts=3

# Custom connection
php artisan rabbit-rs:work --connection=rabbit-rs --queue=high-priority
```

The supervisor spawns child `queue:work` processes, passes the worker index via `RABBIT_RS_WORKER`, and restarts them on exit: a clean exit (0, e.g. `--max-jobs` recycling) restarts immediately without consuming the restart budget, while a non-zero exit is treated as a crash and restarted with exponential backoff up to `--max-restarts` times.

| Option | Description | Default |
| ------ | ----------- | ------- |
| `--connection` | Queue connection name | `rabbit-rs` |
| `--queue` | Queue or worker profile name | `default` |
| `--workers` | Number of child workers | `1` |
| `--max-restarts` | Max restarts per worker | `3` |
| `--backoff` | Base backoff in seconds | `1` |

### Status diagnostics

```bash
# Human-readable
php artisan rabbit-rs:status

# JSON for monitoring
php artisan rabbit-rs:status --format=json
```

Displays pool state, connection generations, in-flight counters, and consumer counts per broker.

### Octane

When Laravel Octane is detected, the driver automatically:

- Closes cached consumers after each request (prevents channel leaks)
- Flushes all pools on worker reload
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
php vendor/bin/phpunit --testsuite="Rabbit RS Laravel"

# Integration tests (requires a running RabbitMQ broker)
php vendor/bin/phpunit --testsuite="Rabbit RS Integration"
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
