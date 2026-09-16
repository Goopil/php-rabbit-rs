# Reference

Reference documentation for the Laravel queue driver: configuration, usage, topology, and operations. The Octane chapter and the recipes live on separate pages ([octane.md](octane.md), [recipes.md](recipes.md)); the everyday path is the [getting started](getting-started.md).

**Contents**

- [Usage](#usage) — dispatch API, events, job class, Horizon
- [Configuration](#configuration) — every connection key, validation
- [Topology](#topology) — declare/verify/external, dead-letter wiring
- [Operations](#operations) — diagnostics, supervisors, Kubernetes, metrics
- [Octane Integration](octane.md) — lifecycle hooks and pitfalls (separate page)
- [Recipes](recipes.md) — topology patterns, broker tuning, capacity planning (separate page)

## Usage

The everyday path is the [getting started](getting-started.md); this page is the reference for the driver's API and runtime behaviour: dispatching, consuming, events, the job class, and Horizon.

### Dispatching jobs

#### Dispatch a job

```php
use App\Jobs\ProcessOrder;

// Dispatch to the default queue
ProcessOrder::dispatch($order);

// Dispatch to a specific queue
ProcessOrder::dispatch($order)->onQueue('orders.high');
```

#### Dispatch with delay

```php
// Delay by 5 minutes
ProcessOrder::dispatch($order)->delay(now()->addMinutes(5));
```

Delayed jobs use the configured delay mode: `auto` verifies the `rabbitmq_delayed_message_exchange` plugin against the management API at connection compile time and publishes through the `x-delayed-message` exchange only when present (otherwise it degrades to `ttl` bucket queues), `plugin` refuses the delayed publish loudly when the management API proves the plugin absent, and `ttl` uses bucketed TTL queues. See [Topology — Delay routing](#delay-routing).

#### Dispatch in bulk

```php
use App\Jobs\ProcessOrder;
use Illuminate\Support\Facades\Queue;

$jobs = [
    new ProcessOrder(1),
    new ProcessOrder(2),
    new ProcessOrder(3),
];

// Bulk dispatch — a single native call for all immediate jobs
Queue::connection('rabbit-rs')->bulk($jobs);
```

#### Raw payloads

```php
// Publish a raw payload (not serialized by Laravel)
Queue::connection('rabbit-rs')->pushRaw($jsonPayload, 'orders.high');
```

### Consuming jobs

#### Standard queue worker

```bash
php artisan queue:work rabbit-rs
```

This uses Laravel's built-in `queue:work` command — the connection is its positional argument. Without `--queue`, the worker consumes the connection's default queue (its `queue` key). The `--queue` option resolves a queue or profile on that connection:

```bash
# Consume the "orders.high" queue defined on the connection
php artisan queue:work rabbit-rs --queue=orders.high
```

The connection compiles to a single worker profile (named after the connection) spanning all its queues — the `queue` key plus every `subscriptions` entry. A single `pop()` call selects the next delivery from any ready subscription using the weighted-fair scheduler.

The `--queue` value is resolved in this order:

1. A queue consumed by the connection (its `queue` key or a `subscriptions` entry's `queue`) — a pop addressed to one queue of a multi-queue connection resolves a dedicated single-queue implicit profile (see [Implicit profiles](#implicit-profiles)), so it never draws from the connection's other queues; a pop addressed to a single-queue connection or to the profile name uses the compiled profile.
2. The connection name (the profile name) — the connection's whole profile, all subscriptions included, is used.
3. Otherwise the name is a plain queue nothing consumes: `pop()` fails with an actionable error telling you to declare the queue in the connection's `queue` key or `subscriptions` (the removed `auto_subscribe` opt-in is rejected at compile time — see [Implicit profiles](#implicit-profiles)).

#### Multi-process supervisor

```bash
php artisan rabbit-rs:work --workers=4
```

The `rabbit-rs:work` command supervises `queue:work` child processes with automatic restart on crash. With no flags it fans out: **one child per rabbit-rs connection**, each consuming every queue defined on its connection (`queue` key first, then `subscriptions` queues); `--workers` spawns children per connection.

Options:

| Option | Description | Default |
|--------|-------------|---------|
| `--connection` | Comma-separated connection names | Every rabbit-rs connection |
| `--queue` | Comma-separated queue names, resolved by definition (connection `queue` key or `subscriptions` alias) | Every defined queue |
| `--workers` | Children spawned per connection (the initial fleet when auto-scaling is configured) | `1` |
| `--max-restarts` | Max restarts per worker | `3` |
| `--backoff` | Base backoff in seconds | `1` |
| `--stop-when-empty` | Once mode: children get `--stop-when-empty`, are never recycled, and the supervisor exits with the highest child exit status once every child has terminated (CI smoke tests) | disabled |
| `--once` | Once mode: children get `--once` (a single job each) under the same one-shot supervision | disabled |
| `--min-workers`, `--max-workers` | Auto-scaling floor and ceiling per connection; without `--max-workers` the fleet is the fixed `--workers` and scaling is off | `1`, — |

Unknown connection or queue names fail with a typed error listing what is available. A queue defined on two targeted connections is consumed on both — see [Worker fan-out](#worker-fan-out) for the full semantics.

Each child runs `queue:work` with a unique worker name. On crash, the supervisor restarts the child with exponential backoff (capped at 60 seconds). On `SIGTERM`/`SIGINT`, the supervisor gracefully stops all children.

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | Clean shutdown (including `SIGTERM`/`SIGINT`) |
| `1` | Max restarts exceeded, or a child crash in once mode (`--stop-when-empty` or `--once`; the highest child exit status is propagated) |

#### Status command

```bash
php artisan rabbit-rs:status
```

Displays pool state (handle, PID, closed), publisher counters (publishes, confirmations, returns, dropped publications, backpressure, reconnects, duplicates), consumer counters (deliveries, acks, rejects), and confirmation/settlement latency percentiles. A non-zero `dropped publications` counter is called out with a warning — publications were dropped on a closed client. For machine-readable output:

```bash
php artisan rabbit-rs:status --format=json
```

The status command is read-only. It does not reconnect or modify topology.

### RabbitMqQueue API

The `RabbitMqQueue` class implements `Illuminate\Contracts\Queue\Queue` and `Illuminate\Contracts\Queue\ClearableQueue`.

#### push

```php
Queue::connection('rabbit-rs')->push(ProcessOrder::class, ['orderId' => 42], 'orders.high');
```

#### pushRaw

```php
Queue::connection('rabbit-rs')->pushRaw($rawPayload, 'orders.high', ['content_type' => 'application/json']);
```

#### later

```php
Queue::connection('rabbit-rs')->later(300, ProcessOrder::class, ['orderId' => 42], 'orders.high');
```

The delay is specified in seconds. Rabbit RS converts it to milliseconds and routes through the delay mode.

#### bulk

```php
$messageIds = Queue::connection('rabbit-rs')->bulk([
    new ProcessOrder(1),
    new ProcessOrder(2),
    new ProcessOrder(3),
], '', 'orders.high');
```

Bulk publishing uses a single native call (`publishBatch`) for all immediate jobs. Jobs marked `dispatchAfterCommit` are deferred to the transaction commit callback.

#### pop

```php
$job = Queue::connection('rabbit-rs')->pop('orders.high');
if ($job !== null) {
    $job->fire();
}
```

`pop()` delegates to the native consumer set. The queue argument is resolved on the connection (see the resolution order above): a queue the connection consumes (`queue` key or `subscriptions`) — scoped to a dedicated single-queue implicit profile when the connection consumes several queues — or the connection name (its whole profile). A plain queue name nothing consumes fails with an actionable error telling you to declare it. A single call selects the next delivery from any ready subscription using the weighted-fair scheduler.

#### size

```php
$depth = Queue::connection('rabbit-rs')->size('orders.high');
```

Returns the message count for the queue. Uses AMQP passive declaration (no management API required).

#### clear

```php
$purged = Queue::connection('rabbit-rs')->clear('orders.high');
```

Purges all messages from the queue and returns the number of jobs removed (the pending count measured before the purge; messages racing the purge are counted but may survive). Requires configuration permissions on the broker. The `ClearableQueue` contract makes `php artisan queue:clear rabbit-rs` available.

#### stats

```php
$stats = Queue::connection('rabbit-rs')->stats();
```

Returns the process-local native pool counters for the connection's pool — the same shape `rabbit-rs:status` prints. Alongside the delivery/settlement counters (`deliveries_total`, `acks_total`, `rejects_total`) it exposes `returns_total` (unroutable mandatory publications) and `dropped_publications_total` (publications dropped on a closed client, never handed off to the broker — drain them before a short-lived process exits when they matter). Counters are per-process by design: they count what *this* process published and settled — see [Reliability — Measuring duplicates](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#measuring-duplicates) for the cross-process signals.

### Events

Rabbit RS dispatches two native events through the Laravel event system:

#### ConnectionStateChanged

Dispatched when a broker connection state changes:

```php
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;

class ConnectionStateListener
{
    public function handle(ConnectionStateChanged $event): void
    {
        Log::info("Broker {$event->broker} state: {$event->state} (generation {$event->generation})");
        // $event->state is 'recovering' or 'ready'
        // $event->generation increments on each successful recovery
    }
}
```

Register in `EventServiceProvider`:

```php
protected $listen = [
    ConnectionStateChanged::class => [
        ConnectionStateListener::class,
    ],
];
```

#### BackpressureDetected

Dispatched when the publisher reaches its capacity:

```php
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;

class BackpressureListener
{
    public function handle(BackpressureDetected $event): void
    {
        Log::warning("Backpressure on {$event->broker}: {$event->inFlight}/{$event->capacity} in flight");
    }
}
```

#### RabbitRsProbeEvaluated

Dispatched by `rabbit-rs:probe` before the exit code is decided. Synchronous listeners may flip `$event->verdict` to `false` to force a probe to fail (maintenance mode, external flags) and pull the pod out of rotation:

```php
use Goopil\RabbitRs\Laravel\Events\RabbitRsProbeEvaluated;

class ProbeMaintenanceListener
{
    public function handle(RabbitRsProbeEvaluated $event): void
    {
        if (Maintenance::active()) {
            $event->verdict = false;
        }
    }
}
```

#### Custom callbacks

You can register custom callbacks directly on the `Pool` instance to replace the default event dispatch:

```php
$pool->onConnectionState(function (string $broker, string $state, int $generation): void {
    // Custom handling
});

$pool->onBackpressure(function (string $broker, int $inFlight, int $capacity): void {
    // Custom handling
});
```

### Job class

`RabbitMqJob` extends `Illuminate\Queue\Jobs\Job` and implements:

- `getJobId()` — returns the stable `message_id` (UUID from Laravel payload)
- `getRawBody()` — returns the raw payload string
- `attempts()` — returns the delivery attempt count (from `x-acquired-count` / `x-delivery-count` headers)
- `delete()` — sends `basic.ack` and releases the delivery handle
- `release($delay)` — sends `basic.reject(requeue=true)` for delay 0, or republicates via the delay mode for delay > 0

The delivery handle is released after a terminal transition (ack, reject, or release) to prevent double-settlement.

### Worker lifecycle

#### Graceful shutdown

The worker handles `SIGTERM` and `SIGINT`. In-flight deliveries are not acknowledged during shutdown — RabbitMQ redelivers them after the connection closes.

Sending `SIGTERM` to the supervisor stops all children gracefully — they finish their current jobs — and the supervisor exits with code 0. There is no separate restart signal: the process supervisor (systemd, Supervisor, Kubernetes) is responsible for restarting the stopped worker.

### Laravel Horizon

Rabbit RS integrates with [Laravel Horizon](https://laravel.com/docs/horizon) so jobs processed via RabbitMQ appear in the Horizon dashboard alongside Redis jobs. RabbitMQ remains the transport; Redis is used by Horizon for job tracking, metrics, and dashboard state. In `config/horizon.php` a Rabbit RS connection is configured exactly like a Redis queue: a supervisor whose `connection` points at it, with its queues.

#### Setup

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
        // Same shape as the Redis supervisor — only the connection changes.
        'supervisor-rabbit' => [
            'connection' => 'rabbit-rs',
            'queue' => ['orders', 'billing'],
            'maxProcesses' => 3,
        ],
    ],
],
```

#### How it works

When `worker=horizon`, the driver uses `Horizon\RabbitMqQueue` which dispatches Horizon events at each job lifecycle stage:

| Stage | Event | When |
|-------|-------|------|
| Before push | `JobPending` | `push()` before sending to RabbitMQ |
| After push | `JobPushed` | `push()` after the broker accepts the message |
| After pop | `JobReserved` | `pop()` returns a job to the worker |
| After delete | `JobDeleted` | `delete()` acknowledges the job |

Horizon stores this metadata in Redis and displays it in the dashboard. The RabbitMQ message itself is the source of truth for delivery — Horizon's Redis state is for observability only.

#### Coexistence with Redis queues

Both Redis and Rabbit RS connections can run in the same Horizon instance. Redis queues use Horizon's native Redis backend; Rabbit RS queues dispatch events that Horizon tracks in Redis while using RabbitMQ as the actual transport.

#### Without Horizon

If Horizon is not installed, set `RABBIT_RS_WORKER=default` (the default). The `horizon` mode requires `laravel/horizon` to be installed.

### Octane

See [Octane](octane.md) for Octane-specific lifecycle hooks and configuration.

## Configuration

Rabbit RS is configured connection-first: every broker, its credentials, its
routes, and its worker profile live on a single **queue connection** in
`config/queue.php`, exactly like Laravel's built-in `redis` and `sqs` drivers.

Two config homes:

| File | Role |
|------|------|
| `config/queue.php` → `queue.connections.*` | **The primary surface.** One connection = one broker/vhost = one native pool. |
| `config/rabbit-rs.php` | Cross-cutting **defaults** merged under every rabbit-rs connection (~50 lines). Publish with `php artisan vendor:publish --tag="rabbit-rs-config"`. |

Nothing is normalized at boot: each connection is compiled **lazily**, when the
queue manager first resolves it. A config typo only fails the driver's use —
never the whole application — and `octane:reload` picks up fresh config
automatically (see [Octane](octane.md#reload-worker-reload)).

### Single broker

Add one connection to `config/queue.php`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => 'default',

        // Broker — one connection = one broker/vhost = one native pool
        'hosts' => '127.0.0.1:5672',
        'vhost' => '/',
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'heartbeat' => 30,
        'tls' => [
            'enabled' => false,
            'ca_cert' => null,
            'client_cert' => null,
            'client_key' => null,
        ],

        // Publication
        'exchange' => 'laravel.jobs',   // null = default exchange (direct-to-queue)
        'routing_key' => '{queue}',     // {queue} placeholder; null = no routing key
        'safety' => 'safe',             // safe | unsafe | blind
        'confirm_timeout' => 30000,     // ms, >= 1000

        // Consumption
        'prefetch' => 1000,             // per subscription channel (see below)
        'wait_timeout' => 30000,        // ms, 1000..86400000

        // Topology (defaults inherited from config/rabbit-rs.php)
        'topology_mode' => 'declare',   // declare | verify | external
        'queue_type' => 'quorum',       // quorum | classic
        'queue_durable' => true,
        'delivery_limit' => null,
        'dead_letter' => null,

        // Framework keys
        'after_commit' => false,
        'block_for' => null,
    ],
],
```

Dispatch and consume:

```bash
php artisan queue:work rabbit-rs
# or the supervised fan-out command:
php artisan rabbit-rs:work
```

Every key above except `driver` and `queue` is optional. A key the connection
omits falls back to the cross-cutting defaults in `config/rabbit-rs.php`
(per sub-key for `tls`, `delay`, and `dead_letter`); keys with no entry there —
`hosts`, `vhost`, `username`, `password`, `exchange`, `routing_key`,
`subscriptions`, `management_url`, `max_attempts`, and the framework keys —
use the driver's built-in defaults (see [Cross-cutting defaults](#cross-cutting-defaults)).
The minimal connection is therefore:

```php
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue' => 'default',
],
```

### Connection reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `driver` | string | — | Must be `rabbit-rs` |
| `queue` | string | — (required) | Default queue name: the derived consumer subscription and the `pop()` target |
| `hosts` | string or string[] | `127.0.0.1:5672` | Comma-separated `host:port` list (a bare host defaults to port 5672); IPv6 must be bracketed (`[::1]:5672`) |
| `vhost` | string | `/` | AMQP virtual host (a distinct vhost = a distinct AMQP connection) |
| `username` | string | `guest` | AMQP username |
| `password` | string | `guest` | AMQP password |
| `tls` | array | package defaults | `enabled`, `ca_cert`, `client_cert`, `client_key` (see [TLS](#tls)) |
| `heartbeat` | int (seconds) | `30` | AMQP heartbeat; positive integer |
| `management_url` | ?string | `null` | Laravel-only: RabbitMQ management API base URL for `rabbit-rs:status` (never sent to the native extension) |
| `exchange` | ?string | `laravel.jobs` | Publishing exchange; `null` publishes through the default exchange |
| `routing_key` | ?string | `{queue}` | `{queue}` is replaced with the queue name at publish time; `null` means no routing key (default-exchange/fanout usage) |
| `safety` | string | `safe` | `safe` (confirms + mandatory), `unsafe` (no confirms, no mandatory — synchronous socket write), `blind` (fire-and-forget) |
| `confirm_timeout` | int (ms) | `30000` | Publisher confirm timeout, minimum `1000`; during a recovery, a publish parked in replay is retried once with a fresh deadline, while a confirm timeout on a live connection stays terminal |
| `flush_interval` | int (ms) | `1` | Age-flush deadline of the publish buffer: a lone publish is flushed to the broker this long after being buffered even if the process never publishes, pops, or flushes again. 0–3,600,000 |
| `prefetch` | int | `1000` | QoS prefetch per consumer channel, 1–65535 |
| `wait_timeout` | int (ms) | `30000` | Transport (broker connection) acquisition deadline, 1000–86400000 — **not** the `pop()` wait; use `block_for` to make `pop()` block for work |
| `max_attempts` | int | `20` | Inclusive cap on resolved delivery attempts before terminal settlement |
| `best_effort` | bool | `false` | Gates `early_ack`/`no_ack` on this connection's subscriptions |
| `topology_mode` | string | `declare` | `declare`, `verify`, `external` — see [Topology](#topology) |
| `queue_type` | string | `quorum` | `quorum` or `classic` |
| `queue_durable` | bool | `true` | Queue durability |
| `delivery_limit` | ?int | `null` | Quorum-queue delivery limit; **requires `dead_letter`** when set |
| `dead_letter` | ?array | `null` | `['exchange' =>, 'queue' =>, 'routing_key' =>]` |
| `delay` | array | package defaults | `mode`, `buckets`, `max_buckets`, `queue_expiry_margin` (see [Delayed messages](#delayed-messages)) |
| `subscriptions` | array | — | Escape hatch replacing the derived subscription (see [Subscriptions escape hatch](#subscriptions-escape-hatch)) |
| `worker` | string | `default` | `default` or `horizon` (framework key) |
| `production_warning` | bool | `true` | Silences the unbounded-redelivery warning for this connection |
| `after_commit` | bool | `false` | Framework key: dispatch after the database transaction commits |
| `block_for` | ?int | `null` | Framework key: seconds to block for new jobs before returning |

`prefetch` applies **per subscription channel**. A worker process consuming
several subscriptions holds one channel per subscription, so its in-flight
total is the **sum of the subscriptions' prefetch values** (for the default
single derived subscription, that is the connection's `prefetch`). N
concurrent workers multiply that per-process total. This is standard AMQP
behavior; size your workers accordingly.

#### `block_for` — the pop wait

`block_for` is a framework key, accepted since 0.1.0 and read by the connector
from the raw connection config (it is not part of the native compilation). It
is an integer number of **seconds**, or `null`; the default `null` resolves to
0 — a **non-blocking pop** that returns immediately when no delivery is ready.

When set, `pop()` blocks up to that window waiting for a delivery:

```php
'rabbit-rs' => [
    // ...
    'block_for' => 3, // seconds; pop() waits up to 3s for work
],
```

Pop-once consumers and tests should set it (1–5 s is a sensible range): with a
non-blocking pop, a `pop()` issued right after a publish can silently miss the
job while the publish is still in flight (buffered or not yet routed), and the
caller reports an empty queue. The value must be a non-negative integer or
`null`; anything else fails with the exact `queue.connections.<name>.block_for`
path. `wait_timeout` is a different knob — the broker-connection acquisition
deadline, not the pop wait.

### Environment strings

Laravel's `env()` returns strings for numbers and flags from `.env`. The
compiler casts them lazily at connection resolution:

- **Booleans** accept `true`, `"1"`, `"true"`, `"on"`, `"yes"` / `false`,
  `"0"`, `"false"`, `"off"`, `"no"`, `""`; `null` falls back to the key's own
  fallback.
- **Integers** accept signed digit strings (`"64"`, `"-1"`), then the existing
  range checks apply.
- Anything else, and any unknown key, throws `InvalidArgumentException` with
  the full config path, e.g. `queue.connections.orders.prefetch`.

The published `config/rabbit-rs.php` uses plain `env()` calls without casts —
put your env wiring wherever it reads best:

```php
// config/queue.php — everything via env
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue' => env('RABBIT_RS_QUEUE', 'default'),
    'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
    'vhost' => env('RABBIT_RS_VHOST', '/'),
    'username' => env('RABBIT_RS_USERNAME', 'guest'),
    'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
    'max_attempts' => env('RABBIT_RS_MAX_ATTEMPTS', 20),
],
```

### Cross-cutting defaults

`config/rabbit-rs.php` holds only defaults that are merged under every
rabbit-rs connection. A key the connection omits is inherited from here — per
sub-key for the three nested sections (`tls`, `delay`, `dead_letter`), so a
connection that sets only `delay.mode` still inherits the package `buckets`.
A connection value — including an explicit `null` — always wins.

| Key | Env hook | Default |
|-----|----------|---------|
| `heartbeat` | `RABBIT_RS_HEARTBEAT` | `30` |
| `tls.enabled` | `RABBIT_RS_TLS` | `false` |
| `tls.ca_cert` | `RABBIT_RS_TLS_CA_CERT` | `null` |
| `tls.client_cert` | `RABBIT_RS_TLS_CLIENT_CERT` | `null` |
| `tls.client_key` | `RABBIT_RS_TLS_CLIENT_KEY` | `null` |
| `safety` | `RABBIT_RS_SAFETY` | `safe` |
| `confirm_timeout` | `RABBIT_RS_CONFIRM_TIMEOUT` | `30000` |
| `prefetch` | `RABBIT_RS_PREFETCH` | `1000` |
| `wait_timeout` | `RABBIT_RS_CONSUMER_WAIT_TIMEOUT` | `30000` |
| `topology_mode` | `RABBIT_RS_TOPOLOGY_MODE` | `declare` |
| `delay.mode` | `RABBIT_RS_DELAY_MODE` | `auto` |
| `delay.buckets` | `RABBIT_RS_DELAY_BUCKETS` | `1,5,30,120` |
| `delay.max_buckets` | `RABBIT_RS_DELAY_MAX_BUCKETS` | `8` |
| `delay.queue_expiry_margin` | `RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN` | `60` |
| `worker` | `RABBIT_RS_WORKER` | `default` |
| `production_warning` | `RABBIT_RS_PRODUCTION_WARNING` | `true` |
| `best_effort` | `RABBIT_RS_BEST_EFFORT` | `false` |
| `probes.path` | `RABBIT_RS_PROBES_PATH` | `storage_path('framework/rabbit-rs/probes')` |

Keys with no per-connection default wiring (`queue_type` = `quorum`,
`queue_durable` = `true`, `delivery_limit` = `null`, `dead_letter` = `null`)
are plain values in the file — set them per connection when you need to vary
them.

Connection-only keys have no entry in this file: `queue`, `hosts`, `vhost`,
`username`, `password`, `exchange`, `routing_key`, `subscriptions`,
`management_url`, `max_attempts`, and the framework keys (`after_commit`,
`block_for`). Wire them with `env()` directly on the connection, as shown
above.

### Multiple brokers and vhosts

A vhost owns a distinct AMQP connection. To consume from or publish to several
brokers or vhosts, define one connection per broker — more brokers is more
connections, the framework's own way of expressing multiple backends:

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

Publish and consume per connection:

```php
BillingJob::dispatch($invoice)->onConnection('billing');
OrdersJob::dispatch($orderId)->onConnection('orders-eu');
```

```bash
# Consume every queue of every rabbit-rs connection (see Worker fan-out)
php artisan rabbit-rs:work

# Consume only the listed connections
php artisan rabbit-rs:work --connection=orders-eu,billing
```

`hosts` accepts a flat comma-separated string (env-friendly:
`"rabbit-1:5672,rabbit-2:5672"`) or an array of such strings. Endpoints are
sorted and Rabbit RS connects to the first reachable host.

#### Composed consumer behavior

A worker subscribed to several queues on one connection gets one composed
consumer: deliveries fan in through a single `pop()` call under weighted-fair
scheduling, and each delivery's ACK/Release/Reject is routed back to its
source. Ordering is guaranteed only within a single queue. A broker that is
recovering does not stop consumption from other connections; when its consumer
set is replaced after recovery, the composed consumer surfaces a one-shot
`Goopil\RabbitRs\ConnectionException` ("broker source replaced by recovery;
re-fetch consumer") — re-fetch the consumer (e.g. `closeConsumers()` on the
queue connector) and the fresh handle re-subscribes without duplicating
subscriptions (see [Reliability — Connection recovery](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#connection-recovery)).

### Subscriptions escape hatch

By default a connection consumes exactly one queue: its `queue` key. Set
`subscriptions` to consume several queues on the same broker with per-subscription
tuning. The alias is the array key; the broker is always this connection
(a subscription cannot cross brokers — use a second connection for that):

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => 'rabbit-1:5672',
    'vhost' => '/orders-eu',
    'username' => 'orders',
    'password' => 'secret',

    'subscriptions' => [
        'critical' => [
            'queue' => 'orders.critical',
            'weight' => 8,
            'prefetch' => 8,
        ],
        'bulk' => [
            'queue' => 'orders.bulk',
            'weight' => 2,
            'prefetch' => 32,
        ],
    ],
],
```

| Field | Default | Description |
|-------|---------|-------------|
| `queue` | — (required) | Broker queue to consume |
| `weight` | `1` | Delivery share vs other subscriptions (1–65535) |
| `prefetch` | connection `prefetch` | QoS prefetch for this subscription |
| `early_ack` | `false` | Requires `best_effort` |
| `no_ack` | `false` | Requires `early_ack` **and** `best_effort` |

Without the escape hatch, one subscription named `default` is derived from the
connection's `queue`. With it, the list replaces the derivation. Rules:
at least one entry, unique queues across aliases, unknown fields rejected.

### Implicit profiles

The implicit `__auto__.{queue}` mechanism remains for one purpose: **multi-queue pop scoping**. A pop addressed to one queue of a multi-queue connection always resolves a dedicated `__auto__.{queue}` consumer instead of the shared profile, so its prefetch and deliveries are not pooled with the other subscriptions — `pop('orders.critical')` never draws from the connection's other queues (and Horizon supervisors popping named queues inherit the same guarantee). The implicit profile is built with subscription defaults (not the compiled subscription's custom prefetch); the compiled profile remains for topology, doctor, and publishing.

`auto_subscribe` itself is removed: pops of plain queue names nothing declares fail with an actionable error telling you to declare the queue on the connection (`queue` key or `subscriptions`). The option was rejected at compile time in v1 (#164-2) because runtime worker-profile registration is not supported — a connection carrying the key (any value, including through stale package defaults) fails compilation with guidance instead of surfacing the native `unknown worker profile` error at first pop.

Prefer declared subscriptions: they control per-queue weights and prefetch, and they are visible to `rabbit-rs:status`.

### Worker fan-out

`rabbit-rs:work` supervises one `queue:work` child **per targeted
connection** (each child gets that connection's whole queue set through the
native weighted-fair scheduler). Cross-connection fairness is process-level,
same as every Laravel driver.

| Flag | Default | Behavior |
|------|---------|----------|
| *(none)* | — | Every rabbit-rs connection's every defined queue (`queue` key first, then `subscriptions` queues) |
| `--connection=a,b` | all | Restrict to the listed connections (unknown → error listing available connections) |
| `--queue=x,y` | all defined | Resolve each name **by definition**: a connection's `queue` key or a `subscriptions` alias. Unknown → error listing all defined queues |
| `--workers=N` | `1` | Children spawned **per connection** (N connections × N workers total) |
| `--max-restarts`, `--backoff` | `3`, `1` | Supervisor crash-loop protection |
| `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` | `60`, `—`, `128`, `—`, `—` | Propagated to each `queue:work` child |
| `--stop-when-empty` | disabled | Once mode: children run once with `--stop-when-empty` and are never restarted; the supervisor exits with the highest child exit status once all children have terminated |

```bash
php artisan rabbit-rs:work
# → Starting 2 worker(s): orders-eu[orders, orders.critical, orders.bulk], billing[invoices]

php artisan rabbit-rs:work --connection=billing
php artisan rabbit-rs:work --queue=critical
php artisan rabbit-rs:work --connection=orders-eu --queue=critical,bulk --workers=4
```

Semantics worth knowing:

- A queue name defined on two targeted connections is **consumed on both**
  (one child per connection).
- Combining `--connection` and `--queue` intersects both filters: only listed
  connections that define the listed queues run.
- Plain `php artisan queue:work rabbit-rs` still works for a
  single connection; multi-driver setups remain N processes, as with every
  Laravel driver.

### TLS

```php
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
        'client_cert' => '/etc/ssl/certs/client.pem',   // optional, enables mTLS
        'client_key' => '/etc/ssl/private/client.key',
    ],
],
```

Or globally through the package defaults with `RABBIT_RS_TLS=true` and the
`RABBIT_RS_TLS_CA_CERT` / `RABBIT_RS_TLS_CLIENT_CERT` / `RABBIT_RS_TLS_CLIENT_KEY`
env hooks — a connection that omits a `tls` sub-key inherits it.

### Delayed messages

Delay configuration is per connection, with `mode`, `buckets`,
`max_buckets`, and `queue_expiry_margin`:

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    // ...
    'delay' => [
        'mode' => 'auto',      // auto | plugin | ttl
        'buckets' => [1, 5, 30, 120],
        'max_buckets' => 8,
        'queue_expiry_margin' => 60,
    ],
],
```

- `auto` — publish delayed messages through the `x-delayed-message` exchange when the broker confirms the plugin (checked once per connection through the management API at connection compile time), degrading to the `ttl` bucket queues when the plugin is absent or unverifiable
- `plugin` — require the plugin: the first delayed publish throws `DelayPluginMissingException` when the management API proves it absent
- `ttl` — always use TTL queue buckets

> **Note:** when `safety` is `blind`, delayed jobs are **not** honored — the
> blind pump bypasses delay routing and publishes immediately. Use `safe` or
> `unsafe` when you need delay routing.

See [Topology — Delay routing](#delay-routing) for details.

### Status monitoring

`php artisan rabbit-rs:status` prints native pool metrics per connection and,
when a connection defines `management_url`, cross-process queue counters
(`delivered`, `acked`, `redelivered`) fetched from the RabbitMQ management API
using the connection's `username`/`password` and `vhost` for basic auth:

```php
'orders-eu' => [
    'driver' => 'rabbit-rs',
    'queue' => 'orders',
    'hosts' => 'rabbit-1:5672',
    'username' => 'orders',
    'password' => 'secret',
    'management_url' => 'http://rabbit-1:15672',
],
```

```bash
php artisan rabbit-rs:status
php artisan rabbit-rs:status --format=json
```

`management_url` is Laravel-only: it is validated on the connection but never
propagated to the native extension. `null` or blank disables the feature. See
[Reliability — Measuring duplicates](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#measuring-duplicates) for
what the counters mean.

### Safety modes

The `safety` setting selects the safety mode; publisher confirms and mandatory
routing are **derived from it**, never set independently:

- `safe` (default) — at-least-once: confirms + mandatory routing. Unconfirmed
  publications are retained in bounded process memory and replayed with their
  original `message_id` across connection recovery.
- `unsafe` — no confirms, no mandatory routing: the publish performs a
  synchronous socket write and returns without outcome tracking. Unroutable
  messages are silently dropped by the broker, and a transport failure after
  the write is a silent loss.
- `blind` — explicit fire-and-forget: publishing hands the message to a
  bounded background pump and returns without waiting for any transport
  outcome. A transport failure after the hand-off is a silent loss. Delayed
  jobs are not honored in this mode.

#### Where unroutable-publish failures surface (safe mode)

In `safe` mode the broker returns every publication it cannot route
(`mandatory` routing). The return is recorded as a definitive failure and
never re-buffered; it surfaces at the earliest of:

- **The publish itself**, when the outcome is already known synchronously:
  `bulk()` and any explicit `flush()` (also `size()`/`clear()`, which flush
  first) throw `QueueException` carrying the message id and the AMQP reply
  code.
- **The next queue operation** otherwise. Publishes are buffered and drained
  in the background (`publisher.flush_interval`, 1 ms default), so a lone
  `push()`
  returns before the broker confirms. The definitive return then surfaces
  from the next `push()`, `flush()`, `size()`, `clear()`, `stats()`, or
  `pop()` (which drains pending publish errors first through
  `drainSettlementErrors()`); `Pool::drainErrors()` reads the raw records
  without throwing.
- **Process teardown, as a guaranteed net.** A process whose final publish
  was returned and that performs no further queue operation (a lone dispatch
  in a CLI one-shot, or an FPM request that never touches the queue again)
  would otherwise take the record with it: `RabbitMqQueue::__destruct()`
  drains pending publish errors and logs each one at `error` level
  (`rabbit-rs: publication outcome never surfaced before process teardown`,
  with the native `kind`, `message_id`, and `message` context). A throw is
  impossible at that point — destructors cannot propagate exceptions — so
  the log entry plus the `returns_total` counter is the floor of the
  contract.

`stats()['returns_total']` counts every mandatory return for the process
lifetime of the pool (`rabbit-rs:status` exposes it), and `rabbit-rs:doctor`
reads the broker's own `return_unroutable` counter on the publish exchange —
cross-process evidence that survives the death of the publishing process.

See [Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#reliability) for the full contract.

### Validation and strict errors

Every config problem throws `InvalidArgumentException` with the exact path,
and only when the affected connection is resolved:

- Unknown keys — on the connection or inside `tls`, `delay`, `dead_letter`,
  `subscriptions` — are rejected (`queue.connections.<name>.<key>: unknown key`).
- `hosts` must contain at least one non-empty endpoint (`host` or
  `host:port`; a bare host gets the default port 5672); empty
  segments (e.g. `"host1:5672,,host2"`) are rejected. Ports must be 1–65535.
- `safety` must be `safe`, `unsafe`, or `blind`; `confirm_timeout` ≥ 1000;
  `wait_timeout` 1000–86400000; `prefetch` and `weight` 1–65535;
  `heartbeat`, `max_attempts`, and delay buckets are positive integers.
- `dead_letter` is **required** when `delivery_limit` is set — without a DLX,
  poison messages are silently dropped after the limit is reached.
- `early_ack` requires `best_effort`; `no_ack` requires `early_ack` and
  `best_effort`.
- `subscriptions` must contain at least one entry, with unique queues across
  aliases.

### Pool reuse and fingerprints

The compiled connection feeds the native pool fingerprint. Two connections
with **identical arrays** compile identically and share the same native pool
within a PHP process (each keeps its own name and callbacks). A different
vhost, host, or credential always produces a different fingerprint — each
vhost gets its own AMQP connection.

### Fork safety

The runtime registry is process-local. After a fork (e.g., `pcntl_fork()`),
the child process detects the PID change and invalidates all inherited
handles. The child creates fresh connections lazily on first use. This is
transparent to the application.

## Topology

Rabbit RS manages RabbitMQ topology through three modes. The mode is set via `topology_mode` in `config/rabbit-rs.php`.

### Topology modes

#### declare (default)

Rabbit RS declares all exchanges, queues, and bindings idempotently. If the existing topology is incompatible (e.g., a queue exists with different arguments), the declaration fails with a permanent error. The connection's publish exchange and its `{queue}` bindings are declared in declare mode and verified in verify mode, so a queue declared by Rabbit RS is reachable from the publisher side; `routing_key: null` publishes through the default exchange and needs no binding.

Use `declare` when Rabbit RS owns the topology and you want it created automatically:

```php
'topology_mode' => 'declare',
```

#### verify

Rabbit RS performs passive declarations to verify that the expected topology exists with the correct properties. No resources are created. If a declaration or property mismatch is detected, the connection fails with a permanent error.

Use `verify` when an external system (Terraform, Puppet, management CLI) provisions the topology and you want to catch drift:

```php
'topology_mode' => 'verify',
```

#### external

Rabbit RS uses the topology without declaring or verifying anything. No AMQP declaration commands are sent. The broker must already have the correct exchanges, queues, and bindings.

Use `external` when you trust the infrastructure and want to avoid any declaration overhead:

```php
'topology_mode' => 'external',
```

### Queue types

#### Quorum queues (default)

Quorum queues are the default and recommended queue type. They provide replicated, durable message storage with Raft consensus:

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => null,
    ],
],
```

Quorum queues support:
- `delivery_limit` — max delivery attempts before dead-lettering (emitted as `x-delivery-limit`); requires `dead_letter` to be configured when set
- Automatic replication across cluster nodes
- Crash recovery without message loss

#### Classic queues

Classic queues are non-replicated and suitable for workloads where durability is less critical:

```php
'topology' => [
    'queue' => [
        'type' => 'classic',
        'durable' => true,
        'delivery_limit' => null,
    ],
],
```

> Classic queues do not support `x-delivery-limit` in all RabbitMQ versions. The `delivery_limit` setting is emitted as a queue argument regardless, but only quorum queues enforce it.

### Exchange and queue declaration

In `declare` mode, Rabbit RS declares:

1. **Exchanges** — the exchange from each route configuration
2. **Queues** — the queue from each subscription
3. **Bindings** — queue-to-exchange bindings using the routing key

The exchange type defaults to `direct` (matching the `{queue}` routing key pattern). Queues are declared as durable, non-exclusive, and non-auto-delete.

#### Recovery order

After a connection recovery, topology is reconciled in deterministic order:

1. Connection and negotiation
2. Channels
3. Exchanges
4. Queues
5. Bindings
6. QoS (prefetch)
7. Consumers
8. Publisher replay (unconfirmed publications)

This order ensures that consumers are only re-registered after their queues and bindings exist.

### DLQ configuration

By default, Rabbit RS does **not** create a dead-letter queue. Dead-lettering must be enabled explicitly:

```php
'topology' => [
    'queue' => [
        'type' => 'quorum',
        'durable' => true,
        'delivery_limit' => 20,
    ],
    'dead_letter' => [
        'exchange' => 'laravel.jobs.dlx',
        'queue' => 'laravel.jobs.dead',
        'routing_key' => 'dead',
    ],
],
```

When `dead_letter` is non-null, Rabbit RS:

1. Declares the dead-letter exchange (`laravel.jobs.dlx`)
2. Declares the dead-letter queue (`laravel.jobs.dead`)
3. Binds the DLQ to the DLX with the specified routing key (`dead`)
4. Sets `x-dead-letter-exchange` and `x-dead-letter-routing-key` on the main queue

The `routing_key` is optional. If set to `null`, the queue name is used as the routing key.

#### How dead-lettering works

When a message exceeds `delivery_limit`, RabbitMQ dead-letters it to the configured exchange. The message arrives in the DLQ with `x-death` headers recording the original queue, reason, and count.

> **Note:** The `delivery_limit` is enforced by quorum queues. Classic queues rely on application-level attempt tracking.

### Delay routing

Rabbit RS supports delayed message delivery via two strategies, selected by the `delay.mode` setting.

#### Auto (default)

```php
'delay' => [
    'mode' => 'auto',
    'buckets' => [1, 5, 30, 120],
    'max_buckets' => 8,
    'queue_expiry_margin' => 60,
],
```

In `auto` mode the driver checks the broker for the `rabbitmq_delayed_message_exchange` plugin (management API overview, once per connection per process — only when a `management_url` is configured). With the plugin present, delayed messages are published through the `x-delayed-message` exchange, same as `plugin` mode (including its declare-mode topology, see below). Without the plugin — or when the plugin state cannot be verified (no `management_url`, or the management API is unreachable) — `auto` degrades to the `ttl` bucket queues at connection compile time, so a deferred job is never routed through the main queue and never silently lost to a missing plugin.

#### Plugin mode

```php
'delay' => [
    'mode' => 'plugin',
],
```

Requires the `rabbitmq_delayed_message_exchange` plugin. Rabbit RS declares an `x-delayed-message` exchange with the underlying exchange type (e.g., `direct`) and publishes delayed messages with the `x-delay` header.

In `declare` mode, Rabbit RS also declares the `rabbit-rs.delayed` exchange (durable, `x-delayed-type: direct`) and binds every subscription queue to it with the queue name as the routing key, so delayed publishes reach each queue without extra provisioning. The declare requires `configure` permission on `rabbit-rs.*`.

In `external` and `verify` modes, the exchange and its bindings are an infrastructure contract that must be provisioned externally: one `rabbit-rs.delayed` exchange per vhost (or `{route-exchange}.delayed` for custom route exchanges), plus a binding from each subscription queue using the queue name as the routing key.

Install the plugin:

```bash
# On RabbitMQ server
rabbitmq-plugins enable rabbitmq_delayed_message_exchange
```

If the plugin is not installed, the exchange declare fails with a permanent error: in `declare` mode the pool connection fails during topology reconciliation, while in `external` and `verify` modes delayed publishes fail terminally with a transport error — the publisher stays ready and all other publishing (delayed or not) keeps working. Use `ttl` mode when the plugin cannot be installed.

On the first delayed publish the driver additionally re-checks the plugin through the management API (verdict cached per connection for the process lifetime): when the API proves the plugin absent, the publish throws `DelayPluginMissingException` instead of losing the message — without the plugin every deferred publish is silently lost. When the plugin state cannot be verified (no `management_url`, or the management API is unreachable), the publish goes through unchanged with a one-time warning, so an unrelated management outage never breaks a working plugin setup.

#### TTL mode (explicit)

```php
'delay' => [
    'mode' => 'ttl',
    'buckets' => [1, 5, 30, 120],
    'max_buckets' => 8,
    'queue_expiry_margin' => 60,
],
```

TTL mode uses a set of bounded delay queues with `x-message-ttl` and dead-letter exchange configurations. Messages are routed to the appropriate bucket based on the requested delay:

- Delays are rounded **up** to the nearest bucket to ensure a job is never delivered before its deadline
- Each bucket has a TTL queue with `x-dead-letter-exchange` pointing back to the original destination
- TTL queues are declared lazily on first use and have a queue expiry (`x-expires`) to avoid unbounded topology growth
- The queue expiry is set to `max_bucket_delay + queue_expiry_margin` seconds

For example, with buckets `[1, 5, 30, 120]`:
- A 3-second delay → bucket `5` (5-second TTL queue)
- A 10-second delay → bucket `30` (30-second TTL queue)
- A 45-second delay → bucket `120` (120-second TTL queue)

#### Release semantics (TTL mode)

TTL mode does not arm a per-message timer. The delayed job is published into a
synthesized **durable quorum bucket queue** named
`rabbit-rs.delay.<hash>.<fingerprint>.<bucket_ms>` — a hash of the destination
plus a fingerprint of the declaring arguments (`x-message-ttl`, `x-expires`,
the dead-letter target) and the bucket. The bucket queue carries
`x-message-ttl` equal to the **bucket size**, so the job is released when that
TTL expires and dead-letters through the connection exchange back to the main
queue, where it is consumed like any other job.

What this means for timing:

- **The actual release time is the quantized bucket, not the requested
  delay.** The job is routed to the smallest bucket that is at least the
  requested delay (a job is never released early), and it waits for that
  bucket's full TTL: with the default buckets `[1, 5, 30, 120]`, a `later(10)`
  releases at ~t+30 s.
- **The bucket is a floor, not a ceiling.** Quorum queues expire TTL messages
  lazily: on an idle broker the message can sit in the bucket queue past its
  TTL until the queue turns again, so the release delay can exceed the bucket
  by an unbounded amount. Only `delay.mode=plugin` — which requires the
  `rabbitmq_delayed_message_exchange` broker plugin — routes each message
  through the `x-delayed-message` exchange at the exact requested delay.
- **In-flight delayed jobs are protected from bucket-queue deletion.** The
  connection periodically re-declares its live bucket queues (`DelayKeepAlive`,
  issue #211), so the `x-expires` idleness window cannot delete a queue that
  still holds messages. The unbounded wait above stays broker lazy-TTL
  semantics — a late release, never a silent loss.
- **Bucket granularity is the tuning knob** (`delay.buckets`): add
  intermediate buckets to tighten quantization, at the cost of one more
  declared queue per bucket.
- **The dead-letter return requires the publish-side binding.** The bucket
  queue dead-letters into the connection exchange with the destination routing
  key — the same (exchange, routing key) pair an immediate publish uses — so
  the main queue must be bound to that exchange with that routing key. The
  delayed-route binding work (issue #205, in flight) covers this path.

### Topology and recovery

After a connection loss and recovery, the `TopologyReconciler` replays the topology plan for the new connection generation. This ensures that exchanges, queues, and bindings exist before consumers are re-registered and publishers resume.

In `external` mode, no reconciliation commands are sent. The infrastructure is expected to be stable.

In `verify` mode, passive declarations are re-issued to detect drift after recovery.

See [Reliability — Recovery](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#connection-recovery) for the full recovery sequence.

## Operations

Operating Rabbit RS in production: diagnostics, supervisor configuration, Kubernetes deployment, and monitoring.

### Diagnostics

#### rabbit-rs:status

The `rabbit-rs:status` command provides a read-only snapshot of the native pool:

```bash
php artisan rabbit-rs:status
```

Output includes:

- **Pool state** — handle ID, PID, closed flag
- **Publisher metrics** — publishes, confirmations, returns, dropped publications (warned when non-zero), backpressure, reconnects
- **Consumer metrics** — deliveries, acks, rejects
- **Latency** — confirmation and settlement latency at p50/p95/p99

For machine-readable output (useful for monitoring and CI):

```bash
php artisan rabbit-rs:status --format=json
```

The status command is read-only. It does not reconnect, modify topology, or consume messages.

#### Verifying the extension

```bash
php --ri rabbit_rs
```

This shows the extension version and configuration. If the extension is not loaded, check your PHP configuration:

```bash
php -m | grep rabbit_rs
```

#### rabbit-rs:doctor

```bash
php artisan rabbit-rs:doctor
# Single connection
php artisan rabbit-rs:doctor --connection=rabbit-rs
```

One-shot health report per rabbit-rs connection, resolved through the same config compilation the driver uses. Each check prints `ok`, `warn`, or `fail`; the command exits non-zero when any check fails (warnings are allowed), which makes it usable in CI:

- **Config & environment** — extension presence and version against the composer constraint; the resolved worker class (with a warning when `worker` is inherited from the package defaults instead of the connection).
- **Broker** — reachability (AMQP connect, auth, vhost; optional management API probe when `management_url` is set), publisher exchange/routing-key alignment, dead-letter wiring, effective safety settings.
- **Publish outcomes** (needs a reachable management API) — reads the publish exchange's broker-side `return_unroutable` counter: cross-process evidence that survives the death of the publishing process. `fail` under `safe` (messages were published as lost — fix the exchange→queue binding), `warn` under `unsafe`/`blind` (fire-and-forget by contract). Silently skipped without a management API; a missing exchange stays the topology check's finding.
- **Dead-letter canary** (needs a reachable management API and a configured `dead_letter`) — publishes a uniquely marked probe, rejects it terminally through a transient consumer, and verifies its arrival through the management API. The verdict is tiered: received on the configured DLQ → `ok`; found only in the doctor-owned `rabbit-rs.canary.*` DLQ (the configured DLQ's backlog is deeper than the 100-message scan window; the foreign count is reported) → `warn`; never delivered → `fail` (dead-lettered messages would vanish — real broker traffic was produced). A contested run — a full scan window, or Horizon consumers configured on the connection — reports `warn` (`CanaryInconclusiveException`) instead of failing. The canary DLQ is purged and deleted after every run.
- **Horizon supervisors** — supervisor/queue alignment with the checked connection.
- **Event listeners** — registration of the driver's events.

#### rabbit-rs:probe

```bash
php artisan rabbit-rs:probe {startup|ready|alive|prestop} [--max-age=5] [--timeout=20]
```

Kubernetes probes over the worker probe statefiles: exit `0` when healthy, `1` otherwise. Workers running `queue:work rabbit-rs` (including Horizon workers) write a small JSON file per PID — `storage/framework/rabbit-rs/probes/{pid}.json` (path: `rabbit-rs.probes.path`) — on every consume-loop turn (throttled to one write per second) and on every state transition, with an atomic write + rename:

```json
{"pid":123,"state":"booting|running|draining","connected":true,"consumed":42,"acked":40,"nacked":2}
```

- The **file mtime is the loop heartbeat** ("the consume loop is still turning" — process existence cannot tell you this). The file is rewritten at most once per second and at most once per loop turn: with the defaults (`block_for=0`, `queue:work --sleep=3`) an idle worker's heartbeat lands every ~3s, so keep `--max-age` above the worker sleep.
- **`connected`** mirrors what the runtime exposes today: the native connection-state callback (`ready` vs `disconnected`/`connecting`/`recovering`); hysteresis lives in the native recovery coordinator, and before the first callback the worker reports `connected=true`.
- Counters come from `Pool::stats()` (`deliveries_total`, `acks_total`, `rejects_total`).
- Statefiles are **kept as post-mortem artifacts**: a dead worker's file ages out of the freshness window and is swept by the writer after an hour, so crashes can be inspected on the pod.

| Probe | Healthy when | Never checks |
|-------|--------------|--------------|
| `alive` | At least one fresh statefile (`mtime < --max-age`) | Broker reachability — **liveness must not depend on the broker**, or a broker outage restart-loops healthy workers |
| `ready` | Every fresh statefile has `connected=true` | — |
| `startup` | Every fresh statefile has `state=running` (first completed loop turn) | — |
| `prestop` | Always exits `0`; signals the fresh workers' PIDs and waits up to `--timeout` for `state ∈ {draining, stopped}` | — |

Aggregation is uniform — zero fresh statefiles fail, otherwise every fresh statefile must be healthy — which covers any number of workers per pod without special cases. The `RabbitRsProbeEvaluated` event (probe name, observed state, mutable `verdict`) is dispatched before the exit code is decided, so synchronous listeners can force a probe to fail (maintenance mode, external flags).

### rabbit-rs:work supervisor

The `rabbit-rs:work` command supervises `queue:work` child processes across connections. With no flags it **fans out**: one `queue:work` child per rabbit-rs connection, each consuming every queue defined on its connection (its `queue` key first, then its `subscriptions` queues); `--workers` spawns children per connection:

```bash
php artisan rabbit-rs:work --workers=4
```

`--queue=x,y` resolves each name **by definition**: a name matches a connection's `queue` key or one of its `subscriptions` aliases, every (connection, queue) pair whose definition matches is consumed, and an unknown name fails with a typed error listing the available queues. Combining `--connection` and `--queue` intersects both filters.

#### Options

| Option | Description | Default |
|--------|-------------|---------|
| `--connection` | Comma-separated connection names | Every rabbit-rs connection |
| `--queue` | Comma-separated queue names, resolved by definition (connection `queue` key or `subscriptions` alias) | Every defined queue |
| `--workers` | Child workers per connection (the initial fleet when auto-scaling is configured) | `1` |
| `--max-restarts` | Max restarts per worker before giving up | `3` |
| `--backoff` | Base backoff in seconds (doubles on each restart, max 60) | `1` |
| `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` | Propagated to each `queue:work` child | `60`, `—`, `128`, `—`, `—` |
| `--stop-when-empty` | Once mode: children run once (with `--stop-when-empty`) and are never recycled; the supervisor exits with the highest child exit status once all children have terminated | disabled |
| `--once` | Once mode: children get `--once` (a single job each) under the same one-shot supervision; mutually exclusive with `--stop-when-empty` | disabled |
| `--min-workers` | Auto-scaling floor per connection | `1` |
| `--max-workers` | Auto-scaling ceiling per connection; without it the fleet is the fixed `--workers` and scaling is off | — |
| `--scale-cooldown` | Minimum seconds between two scaling passes | `3` |
| `--scale-idle` | Seconds of continuous empty queues before releasing idle workers | `30` |
| `--rabbit-rs-worker` | Worker index (set by the supervisor, not by users) | — |

#### Auto-scaling

`rabbit-rs:work` can grow and shrink its fleet per connection, driven by the broker's queue depth. Scaling is opt-in: without `--max-workers` the fixed `--workers` fleet runs exactly as before.

```bash
php artisan rabbit-rs:work --min-workers=1 --max-workers=8
```

One decision per connection, per pass (a pass runs at most every `--scale-cooldown` seconds):

- **Scale up** when the ready depth exceeds twice the live workers — at most 2 children per pass, never beyond `--max-workers`.
- **Scale down** (long-running mode only) after the depth has stayed at zero for the whole `--scale-idle` window (hysteresis, so a queue draining in a burst does not flap the fleet) — at most 2 children per pass, never below `--min-workers`. The idlest children (highest index) receive a non-blocking `SIGTERM` and their slot is removed without touching the crash-restart budget (downscaling is not a crash); a child that outlives the 15 s grace period is escalated to `SIGKILL`.
- In once mode (`--once` / `--stop-when-empty`) scaling is admission-only: children self-terminate and the supervisor admits more while the depth justifies it — it never signals a child. When the fleet drains, a final depth check re-arms the initial fleet while the broker still reports work; that check re-probes the depth **uncached** once before concluding (the sampler's 2 s memoization window could otherwise report a stale 0 while the broker still held work), and a fully failed fresh probe retries within the existing re-arm budget instead of reporting a drained plan. The re-arm budget renews on **observed progress**: a clean child exit (that child consumed a job) or a decrease of the reported depth between re-arms. Clean exits are the trustworthy signal — quorum-queue gauges lag seconds behind consumption, so `--once` drains queues far deeper than its own fleet even while the gauge sits flat. The absolute cap (3 re-arms) only binds a fleet producing neither clean exits nor a decreasing gauge — a crash loop (crashes propagate as the command's exit status) or a gauge that never converges. For an **authoritative** drain use `--stop-when-empty`: its children observe emptiness directly and never trust the gauge. Caveat on quorum queues: `messages_ready` is reported lazily (Raft/stats convergence takes seconds after a burst), so the depth signal can still under-read right after a large publish — another reason drains belong on `--stop-when-empty`.

The depth is sampled per queue from the first available source:

- **Management API** — when the connection configures `queue.connections.<name>.management_url` (+ credentials), the sampler reads `messages_ready` over HTTP: zero AMQP in the supervisor, and the only source that still counts after every worker has exited (the one-shot final depth check).
- **Passive native probe** — otherwise, `Pool::size()` (the same passive declare the doctor and topology commands use): no management plugin required. While the sampler lives the supervisor holds one extra AMQP connection per connection, and each lookup blocks up to the connection's socket timeout — bounded, at the `--scale-cooldown` cadence. The supervisor never publishes, so its probe pools carry no publish buffer.

Either way, an unreadable depth (request failure, unreachable broker, missing queue, extension absent) leaves that connection silently on a static fleet — no warning, because the sources are optional. A configured-but-failing management endpoint disables scaling for that connection rather than cascading into the native probe. Note that children are fresh processes (never forks), so the probe pools the supervisor owns are safe from inheritance concerns by construction.

Caveat: with a `block_for > 0` driver config, a child parked inside the extension's blocking `next()` may not observe `SIGTERM` promptly. Run auto-scaling with `block_for=0` (the default) so released children exit promptly.

One-shot patterns for CI and cron:

```bash
# CI smoke test: process whatever is pending, fail on a crashed child, exit clean.
php artisan rabbit-rs:work --stop-when-empty

# Cron drain: a single job per child, more children while the queues are deep,
# exit when everything is consumed.
php artisan rabbit-rs:work --once --min-workers=2 --max-workers=8
```

#### Signal handling

| Signal | Behavior |
|--------|----------|
| `SIGTERM` | Graceful shutdown — stop all children, wait for current jobs |
| `SIGINT` | Same as `SIGTERM` |

#### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Clean shutdown (including `SIGTERM`/`SIGINT`) |
| `1` | Max restarts exceeded, or a child crash in once mode (`--stop-when-empty` or `--once`; the highest child exit status is propagated) |

#### How it works

1. The supervisor spawns one child per targeted connection (× `--workers`), each running `php artisan queue:work <name> --queue=<q1,q2>` (the connection is `queue:work`'s positional argument)
2. Each child gets a unique `--name=worker-{i}` and the `RABBIT_RS_WORKER_INDEX={i}` environment variable; dynamically spawned children continue the index sequence so identities never collide
3. The supervisor monitors child processes every 100ms
4. If a child exits with a non-zero code (a crash), the supervisor waits (backoff seconds) and restarts it; a clean exit (0, e.g. `--max-jobs` recycling) restarts the child immediately and resets its crash budget. In once mode (`--stop-when-empty` or `--once`), neither happens: children run once and the supervisor exits with the highest child exit status once all children have terminated (CI smoke-test mode)
5. On `SIGTERM`/`SIGINT`, the supervisor sends `SIGTERM` to each child and waits up to 10 seconds
6. With auto-scaling configured, the loop samples the broker depth every `--scale-cooldown` seconds and applies the [Auto-scaling](#auto-scaling) policy per connection

### Supervisor (systemd) configuration

For production, run `rabbit-rs:work` under systemd or Supervisor to ensure it restarts on crash.

#### systemd

```ini
[Unit]
Description=Rabbit RS Worker
After=network.target rabbitmq-server.service

[Service]
Type=simple
User=www-data
Group=www-data
WorkingDirectory=/var/www/html
ExecStart=/usr/bin/php artisan rabbit-rs:work --workers=4 --max-restarts=0
Restart=always
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=rabbit-rs-worker

# Resource limits
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

Set `--max-restarts=0` to disable the internal restart limit and let systemd handle restarts.

#### Supervisor

```ini
[program:rabbit-rs-worker]
command=php /var/www/html/artisan rabbit-rs:work --workers=4 --max-restarts=0
directory=/var/www/html
user=www-data
autostart=true
autorestart=true
stopwaitsecs=30
stopasgroup=true
killasgroup=true
redirect_stderr=true
stdout_logfile=/var/log/rabbit-rs/worker.log
```

A complete Supervisor config is provided in [`examples/laravel/worker-supervisor.conf`](https://github.com/Goopil/php-rabbit-rs/blob/main/examples/laravel/worker-supervisor.conf).

### Kubernetes deployment

#### Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rabbit-rs-worker
spec:
  replicas: 3
  selector:
    matchLabels:
      app: rabbit-rs-worker
  template:
    metadata:
      labels:
        app: rabbit-rs-worker
    spec:
      containers:
        - name: worker
          image: your-app:latest
          command: ["php", "artisan", "rabbit-rs:work", "--workers=2"]
          env:
            - name: RABBIT_RS_HOSTS
              value: "rabbitmq-0:5672,rabbitmq-1:5672,rabbitmq-2:5672"
            - name: RABBIT_RS_VHOST
              value: "/production"
            - name: RABBIT_RS_USERNAME
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: username
            - name: RABBIT_RS_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: rabbitmq-credentials
                  key: password
          lifecycle:
            preStop:
              exec:
                command: ["php", "artisan", "rabbit-rs:probe", "prestop", "--timeout=20"]
          startupProbe:
            exec:
              command: ["php", "artisan", "rabbit-rs:probe", "startup"]
            periodSeconds: 5
            failureThreshold: 30
          livenessProbe:
            exec:
              command: ["php", "artisan", "rabbit-rs:probe", "alive", "--max-age=5"]
            periodSeconds: 10
            failureThreshold: 3
          readinessProbe:
            exec:
              command: ["php", "artisan", "rabbit-rs:probe", "ready", "--max-age=5"]
            periodSeconds: 5
            failureThreshold: 2
          resources:
            requests:
              cpu: 500m
              memory: 256Mi
            limits:
              cpu: 2000m
              memory: 512Mi
      terminationGracePeriodSeconds: 60
```

#### Key considerations

- **`terminationGracePeriodSeconds`** — must exceed the `prestop` `--timeout` plus the drain budget (longest job duration + shutdown time); with the defaults above, 60s leaves ~40s of drain budget after the 20s prestop wait
- **Replicas** — each pod runs its own PHP process with its own connection pool; RabbitMQ handles load balancing across consumers
- **Resource limits** — each worker process uses ~50-100 MB; account for `--workers` multiplied by per-worker memory
- **Probes** — `rabbit-rs:probe` reads the worker statefiles (see [Diagnostics — rabbit-rs:probe](#rabbit-rsprobe)): `startup` gates pod start until workers completed their first consume loop, `alive` restarts workers whose consume loop stopped turning, `ready` pulls the pod out of Services when `connected` drops, and `prestop` starts the drain before the container's SIGTERM
- **Keep `--max-age` above the worker heartbeat cadence** — the statefile is rewritten at most once per loop turn; with the defaults (`block_for=0`, `queue:work --sleep=3`) that is every ~3s

#### Graceful shutdown in Kubernetes

Kubernetes sends `SIGTERM` to the container's PID 1. The supervisor handles this signal, stops child workers gracefully, and exits with code 0. The `preStop` hook (`rabbit-rs:probe prestop`) signals the workers directly and waits for them to drain **before** that SIGTERM, which is why `terminationGracePeriodSeconds` should exceed the prestop `--timeout` plus the drain budget (maximum job duration plus shutdown time).

The `prestop` hook always exits `0`: whether or not the workers finished draining, Kubernetes proceeds with SIGTERM and the normal graceful-shutdown path.

### Monitoring with Prometheus

Rabbit RS does not include a Prometheus exporter in V1, but the status command provides the metrics needed. You can scrape them with a custom exporter or sidecar.

For operating on these signals — incident playbooks, example alert rules, and a Grafana dashboard definition — see the monorepo's `docs/operations/` ([runbook.md](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/operations/runbook.md), [alerts.md](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/operations/alerts.md), [dashboard.json](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/operations/dashboard.json)).

#### Available metrics

| Metric | Description |
|--------|-------------|
| `publishes_total` | Total published messages |
| `confirmations_total` | Total broker confirmations (ACK and Nack) |
| `returns_total` | Total mandatory returns (unroutable) |
| `backpressure_total` | Times publisher capacity was reached |
| `reconnects_total` | Total connection recoveries |
| `deliveries_total` | Total deliveries received |
| `acks_total` | Total consumer ACKs |
| `rejects_total` | Total consumer rejects |
| `duplicates_total` | Deliveries the broker flagged as redeliveries (per-process; see [Reliability — Measuring duplicates](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#measuring-duplicates)) |
| `dropped_publications_total` | Publications discarded without confirmed delivery (deadline-expired flush retries, un-attempted batches on a closing pool, unconfirmed leftovers at teardown) |
| `dropped_error_records_total` | Publish error records evicted from the bounded drain queue before they could be read |
| `publication_retries_total` | Publications whose deadline expired during a recovery suspension and were re-armed once |
| `publish_buffered` | Publications currently parked in the publish buffer |
| `publish_buffered_bytes` | Cumulative payload bytes currently parked in the publish buffer |
| `confirmation_latency_p50/p95/p99` | Publisher confirmation latency (ms) |
| `settlement_latency_p50/p95/p99` | Consumer settlement latency (ms) |

The snapshot also carries pool-handle state (`closed`, `pid`, `handle`) — see the `Pool::stats()` stub in `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` for the full shape.

#### Sidecar exporter

```yaml
# A simple sidecar that polls rabbit-rs:status --format=json
# and exposes /metrics in Prometheus format
apiVersion: v1
kind: ConfigMap
metadata:
  name: rabbit-rs-exporter
data:
  exporter.sh: |
    #!/bin/bash
    while true; do
      php artisan rabbit-rs:status --format=json > /tmp/stats.json
      sleep 5
    done
```

Alternatively, listen for the `ConnectionStateChanged` and `BackpressureDetected` events and push metrics to your monitoring system. Native events fire during publish and consume operations (`publish()`, `publishBatch()`, `flush()`, `drainErrors()`, consumer `next()`/`tryNext()`/`nextBatch()`, and `stats()`), so no polling is required:

```php
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;

Event::listen(ConnectionStateChanged::class, function (ConnectionStateChanged $e) {
    // Push to Prometheus, Datadog, etc.
});

Event::listen(BackpressureDetected::class, function (BackpressureDetected $e) {
    // Alert on backpressure
});
```

#### RabbitMQ-native metrics

For cluster-level metrics (queue depth, consumer count, node health), use the [RabbitMQ Prometheus exporter](https://github.com/rabbitmq/rabbitmq-prometheus-plugin) that ships with RabbitMQ.

### Backpressure detection and response

Backpressure occurs when the publisher's bounded capacity is reached. This happens when the broker cannot confirm publications as fast as they are produced.

#### Detection

The `BackpressureDetected` event is dispatched with:

- `broker` — the broker name
- `inFlight` — current in-flight publications
- `capacity` — maximum capacity

#### Response strategies

1. **Reduce publish rate** — batch jobs, add delays between batches, or use rate limiting
2. **Scale workers** — add more consumer processes to drain queues faster
3. **Check broker health** — high backpressure may indicate broker overload or network issues
4. **Monitor confirmation latency** — rising `confirmation_latency_p95/p99` indicates broker saturation

#### Backpressure vs. connection loss

Backpressure is not an error — the publisher continues accepting commands, but new publish calls receive a `BackpressureException` when capacity is full. This is a signal to slow down, not a failure. Connection loss, by contrast, triggers the recovery and replay mechanism.

See [Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#reliability) for the full publisher safety model.
