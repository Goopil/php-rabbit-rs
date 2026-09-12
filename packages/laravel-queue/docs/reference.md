# Reference

Reference documentation for the Laravel queue driver: configuration, usage, topology, operations, Octane, and recipes. The everyday path is the [getting started](getting-started.md).

**Contents**

- [Usage](#usage) — dispatch API, events, job class, Horizon
- [Configuration](#configuration) — every connection key, validation
- [Topology](#topology) — declare/verify/external, dead-letter wiring
- [Operations](#operations) — diagnostics, supervisors, Kubernetes, metrics
- [Octane Integration](#octane-integration) — lifecycle hooks and pitfalls
- [Recipes](#recipes) — topology patterns, broker tuning, capacity planning

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

Delayed jobs use the configured delay mode: `auto` and `plugin` publish through the `x-delayed-message` exchange, `ttl` uses bucketed TTL queues (use `ttl` when the plugin is not installed). See [Topology — Delay routing](#delay-routing).

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

Displays pool state (handle, PID, closed), publisher counters (publishes, confirmations, returns, backpressure, reconnects, duplicates), consumer counters (deliveries, acks, rejects), and confirmation/settlement latency percentiles. For machine-readable output:

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

See [Octane](#octane-integration) for Octane-specific lifecycle hooks and configuration.

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
automatically (see [Octane](#reload-worker-reload)).

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
        'prefetch' => 64,               // per subscription channel (see below)
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
| `prefetch` | int | `64` | QoS prefetch per consumer channel, 1–65535 |
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
| `prefetch` | `RABBIT_RS_PREFETCH` | `64` |
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

- `auto` — publish delayed messages through the `x-delayed-message` exchange (same as `plugin`); use `ttl` when the plugin is not installed
- `plugin` — require the plugin; fail if it is not installed
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

In `auto` mode, delayed messages are published through the `x-delayed-message` exchange, same as `plugin` mode (including its declare-mode topology, see below). Use `ttl` mode when the `rabbitmq_delayed_message_exchange` plugin is not installed.

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
- **Publisher metrics** — publishes, confirmations, returns, backpressure, reconnects
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

One-shot health report per rabbit-rs connection, resolved through the same config compilation the driver uses. Each check prints `ok`, `warn`, or `fail`; the command exits non-zero when any check fails (warnings are allowed), which makes it usable in CI. Checks cover: extension presence and version against the composer constraint, the resolved worker class (with a warning when `worker` is inherited from the package defaults instead of the connection), broker reachability (AMQP connect, auth, vhost; optional management API probe when `management_url` is set), publisher exchange/routing-key alignment and dead-letter wiring, effective safety settings, Horizon supervisors, and event listeners.

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
- In once mode (`--once` / `--stop-when-empty`) scaling is admission-only: children self-terminate and the supervisor admits more while the depth justifies it — it never signals a child. When the fleet drains, a final depth check re-arms the initial fleet while the broker still reports work (bounded to 3 re-arms, guarding the late-async-flush race where a publisher's last messages land just after the queue looked empty).

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

## Octane Integration

Rabbit RS integrates with [Laravel Octane](https://laravel.com/docs/octane) to support long-lived worker processes. This chapter covers the lifecycle hooks, configuration, and pitfalls.

### How Octane lifecycle hooks work

Octane keeps a PHP process alive across many requests. This means the native Rabbit RS pool (and its AMQP connections) persists between requests, which is beneficial for performance — but it requires careful cleanup to prevent resource leaks.

Rabbit RS hooks into three Octane lifecycle events:

#### flush (per-request)

Called after each request via `$app->terminating()`:

- **Closes cached consumers** on all resolved `RabbitMqQueue` connections
- Does **not** close the native pool — connections are reused across requests
- Prevents AMQP channel leaks that would accumulate if consumers were never closed

```php
// Triggered automatically
OctaneLifecycle::flush();
```

#### reload (worker reload)

Called when Octane reloads the worker (e.g., after `php artisan octane:reload`):

- Closes cached consumers on all resolved queues
- **Flushes the pool factory** — all native pools are closed and recreated on next use
- **Forgets the queue manager's resolved connections** — the next request
  recompiles every rabbit-rs connection from the current config, so broker and
  credential rotation via env variables takes effect immediately (no stale
  brokers serving the boot-time snapshot)

Because connections are compiled lazily at resolution time (see
[Configuration](#configuration)), `octane:reload` picks up fresh config
automatically — no manual normalization step exists or is needed.

```php
// Triggered by WorkerReload event
OctaneLifecycle::reload();
```

#### stop (worker shutdown)

Called when the Octane worker stops:

- Closes cached consumers
- Flushes the pool factory — all native pools are closed
- Ensures clean shutdown of AMQP connections

```php
// Triggered by WorkerStopping event
OctaneLifecycle::stop();
```

### Event registration

The service provider registers the hooks when Octane is detected:

```php
// In RabbitMqServiceProvider::registerOctaneLifecycle()
if (! class_exists(\Laravel\Octane\Octane::class)) {
    return;
}

$app->terminating(fn () => $lifecycle->flush());
$events->listen(WorkerReload::class, fn () => $lifecycle->reload());
$events->listen(WorkerStopping::class, fn () => $lifecycle->stop());
```

Octane is an optional dependency. If `laravel/octane` is not installed, the hooks are not registered, and the package works normally with FPM and CLI.

### Why not to retain Request or service-container state

Rust threads inside Rabbit RS never retain Zend values, PHP objects, callbacks, Request instances, or service-container references. They only handle owned Rust data (strings, bytes, numbers, structs).

This is critical for Octane because:

1. **Request objects are recycled** — Octane reuses Request instances between requests; retaining a reference would leak stale request state
2. **Service container is reset** — the container is flushed between requests; references to old bindings are invalid
3. **Callbacks must be re-invoked on the PHP thread** — Rabbit RS invokes registered callbacks (like `onConnectionState`) synchronously during `stats()` or other PHP-side operations, never from a Rust thread

#### What this means for your code

- **Do not** store Rabbit RS `Pool` or `Consumer` objects in statics or singletons that persist across requests
- **Do not** pass Request objects, user models, or session data to publish/consume calls — use owned data (strings, arrays, serializable values)
- **Do** rely on the service container to resolve fresh instances each request
- **Do** let the Octane lifecycle hooks clean up consumers between requests

### Configuration for Octane

#### queue.php

Add the Rabbit RS connection to `config/queue.php` as usual — one connection
= one broker/vhost = one native pool:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'after_commit' => false,
    ],
],
```

Every other key falls back to the package defaults in
`config/rabbit-rs.php` — see [Configuration](#configuration).

#### Octane configuration

No special Octane configuration is needed. Rabbit RS is automatically detected and the lifecycle hooks are registered.

```bash
# Start Octane with FrankenPHP
php artisan octane:start --server=frankenphp

# Start with RoadRunner
php artisan octane:start --server=roadrunner

# Start with Swoole
php artisan octane:start --server=swoole
```

#### Supported Octane servers

Every server below is exercised by the real-server certification harness
`scripts/test-octane-runtime.sh --server=<name>`: a live Laravel 12 app
(`tests/Runtime/app`) publishes with no follow-up operation, parks the
publications in the native publish buffer, and the harness asserts the
reload and graceful-stop flush paths deliver them without loss
(scenario detail in the script header). Every status below except Swoole
is backed by a local run of this harness; the RoadRunner `octane-runtime`
job in `.github/workflows/ci.yml` and the four-server matrix in
`.github/workflows/nightly.yml` are wired but have not run yet (first
runs land once these workflows are pushed).

| Server | Status |
|--------|--------|
| RoadRunner | Certified — full scenario green locally (real-server harness; pinned `rr` v2025.1.15, sha256-verified); PR CI + nightly runs pending first push. `octane:reload` recycles workers in place; graceful stop flushes; no loss; drain to zero |
| FrankenPHP | Certified — full no-loss scenario green locally (pinned `dunglas/frankenphp:php8.4` digest, ZTS in-image extension build); nightly run pending first push. Documented availability note: upstream octane's reload shuts the whole frankenphp app down instead of recycling workers in place; the workers flush on the way out, so no data is lost and the harness restarts the server across the reload |
| Open Swoole | Certified — full scenario green locally: the reload flush path is certified (no loss) and the harness asserts the stop loss explicitly; nightly run pending first push. Documented upstream limit: `octane:stop` SIGKILLs workers (laravel/octane Swoole `ServerProcessInspector::stopServer`), so publications parked at stop are lost |
| Swoole | Harness delivered; CI verification pending first nightly run (requires ext-swoole, which the certification dev machine could not build) |

The Swoole-family stop behavior is an upstream laravel/octane property,
not a Rabbit RS one: SIGKILL gives PHP no shutdown callback to run. The
same publications survive `octane:reload` on those servers. Do not rely
on `octane:stop` for a loss-free drain on Swoole/Open Swoole — drain via
`octane:reload` or a CLI worker first.

#### Worker count

On process-based runtimes, each Octane worker is a separate PHP process with its own native pool; connections are not shared between workers. Thread-based runtimes (e.g. FrankenPHP workers) share one OS process, and the native runtime registry is process-local — prefer process-based workers for hard pool isolation. Set the worker count based on your CPU cores and RabbitMQ connection limits:

```bash
# Start with 4 workers (each has its own connection pool)
php artisan octane:start --workers=4
```

#### Flushing the pool

If you need to force-close all connections (e.g., before a deployment), use:

```bash
php artisan octane:reload
```

This triggers `WorkerReload`, which flushes all pools **and forgets the
resolved queue connections**. New requests create fresh AMQP connections and
recompile every connection from the current config — env-based broker or
credential rotation takes effect without a restart.

### Fork safety

Octane with Swoole/Open Swoole may use coroutines or fork workers. Rabbit RS detects PID changes after a fork and invalidates all inherited handles. The child process creates fresh connections lazily on first use.

This is transparent to the application — no special configuration is needed.

### Consumer cleanup

The `RabbitMqQueue` class caches `Consumer` instances per worker profile. The `flush()` hook calls `closeConsumers()` on all resolved queues, which:

1. Calls `close()` on each cached consumer
2. Clears the consumer cache
3. The next request creates fresh consumers on demand

This prevents AMQP channel leaks across requests. Without this cleanup, each Octane request would accumulate consumer channels without closing them.

#### PHP-side destruct

As a safety net, `RabbitMqQueue` has a `__destruct()` that calls `closeConsumers()`. The native `ConsumerHandle` also has a `Drop` implementation that sends a best-effort `Close` to the actor. These ensure cleanup even if the lifecycle hooks are not invoked.

### Connection reuse

The native pool (AMQP connections and channels) is **not** closed between requests. It is only closed on `reload()` and `stop()`. This provides optimal performance:

- **Per-request**: consumers are closed and recreated (cheap)
- **Per-worker**: connections are reused (expensive to recreate)
- **Per-reload**: everything is flushed and recreated (clean state)

## Recipes

Task-oriented guides on top of the reference documentation:

| Recipe | Use it when |
|---|---|
| [Topology patterns](#recipe-topology-patterns) | Mapping Laravel workloads to work-queue / pub-sub / delayed / dead-letter wiring, and gating that topology in CI |
| [Broker tuning](#recipe-broker-tuning) | Choosing queue types, broker watermarks, and prefetch for predictable behavior under load |
| [Capacity planning](#recipe-capacity-planning) | Sizing workers, queues, and brokers from the measured evidence instead of guesses |

### Recipe: Topology patterns

Which RabbitMQ pattern fits which Laravel workload, how each one maps to
Rabbit RS configuration, and how to gate the resulting topology in CI. The
mechanics (modes, declarations, recovery order) live in
[Topology](#topology).

#### Pattern selection

| Pattern | Use when | Rabbit RS wiring |
|---|---|---|
| Work queue (competing consumers) | Background jobs of one kind | The default: one connection, one `queue` key, `--workers=N` |
| Weighted multi-queue | Job classes with different latency needs | `subscriptions` with `weight` / per-subscription `prefetch` |
| Pub/sub (fan-out) | One event, several independent consumer groups | A topic exchange plus one subscription queue per group, each bound with its own routing key |
| Delayed jobs | `Job::dispatch()->delay(...)` | `delay.mode: auto` — a documented alias for the delayed-message plugin; no TTL fallback (an unroutable delay fails terminally). `ttl` remains available as an explicit mode |
| Request/reply (RPC) | Service-to-service call/answer | Not yet built — milestone M3, see the [ROADMAP](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/plans/ROADMAP.md) |

One rule cuts across all patterns: **jobs must be idempotent.** The
at-least-once contract redelivers anything not acknowledged, so an extra copy
is normal, counted, and possible after any reconnect
([Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#duplicates)).

#### Work queue — the default

A single queue with competing consumers is what `queue:work` semantics mean,
and the default config is exactly that:

```php
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue'  => 'orders',
    'hosts'  => [['host' => 'rabbit-1', 'port' => 5672]],
],
```

Scale by adding supervisor children, not prefetch: `php artisan rabbit-rs:work
--connection=rabbit-rs --workers=4` spawns four children that each consume the
connection's whole queue set through the weighted-fair scheduler. Prefetch
tuning (see [Broker tuning](#prefetch-driver-side--the-highest-leverage-knob)) controls
buffering per worker, not parallelism.

#### Pub/sub — several consumer groups, one event

Publish to a topic exchange and let each consumer group own a queue bound with
the routing keys it cares about. In Rabbit RS, every subscription is a queue
on the connection's exchange:

```php
'subscriptions' => [
    'audit'    => ['queue' => 'audit.trail',    'weight' => 1],
    'billing'  => ['queue' => 'billing.events', 'weight' => 4],
],
```

Each subscription gets its own dedicated channel and prefetch. Groups that
should never slow each other down belong on separate *connections* — a
subscription cannot cross brokers.

#### Dead-lettering poison safely

Two rules from the reference docs, repeated here because they bite in
production:

- `delivery_limit` (quorum queues) caps redeliveries — **`dead_letter` MUST be
  configured when it is set**, or poison messages are silently dropped after
  the limit.
- Without any dead-letter config, Rabbit RS creates no DLQ at all.

```php
'queue_type'    => 'quorum',
'delivery_limit' => 20,
'dead_letter'   => ['exchange' => 'dead-letters', 'queue' => 'failed-jobs'],
```

#### Gate the topology in CI

Make the broker contract a deploy check, not a hope:

```bash
# Read-only: verify every subscription queue, the DLX, its binding,
# and each queue's x-queue-type (management_url required for the latter)
php artisan rabbit-rs:topology --connection=rabbit-rs

# Full health report: extension version vs composer constraint, broker
# reachability, publisher wiring, safety summary — CI-friendly exit codes
php artisan rabbit-rs:doctor
```

Both exit non-zero on failure and name the exact config path of anything
missing — wire them into your deploy pipeline before the workers roll. In
`declare` mode, `rabbit-rs:topology --fix` declares missing items through a
transient consumer; in `verify`/`external` mode it is refused without
`--force`, because those modes promise externally managed topology.

#### Which `topology_mode` per environment

| Environment | Mode | Why |
|---|---|---|
| Local / staging, driver owns topology | `declare` | DDL is idempotent and matches the config |
| Production with IaC (Terraform, management CLI) | `verify` | Catches drift without creating anything |
| Frozen platform, topology fully provisioned elsewhere | `external` | Zero declaration traffic |

Whatever the mode, `rabbit-rs:topology` verifies the same promises against the
live broker — see [Topology Mode](#topology-modes).

### Recipe: Broker tuning

The knobs that matter for a Laravel job pipeline live on two sides: the broker
(`rabbitmq.conf`, policies) and the driver (connection config). Listed in the
order they usually bite. Queue-type mechanics and declarations live in
[Topology](#topology).

#### Queue type: quorum by default, classic when replication is not needed

`quorum` (the Rabbit RS default) is replicated with Raft consensus and
enforces `delivery_limit`; `classic` is a single-node durable queue — cheaper
per message, but a node loss takes its queues with it.

Rule of thumb: quorum for anything a job pipeline depends on (the default
exists so you have to *opt out*, not in); classic only for ephemeral,
high-churn queues where replication cost dominates and loss is acceptable by
design.

#### Broker watermarks: what happens when the node is stressed

- `vm_memory_high_watermark` (default 60 % of RAM): when the node crosses it,
  the broker **blocks publishers**. With confirms on, this surfaces in Rabbit
  RS as rising confirm latency and `BackpressureDetected` events (the driver's
  bounded publish buffer fills) — never as silent loss.
- `disk_free_limit`: same publisher-blocking behavior when free disk falls
  below it.

Action: alert on `BackpressureDetected` and on the node's memory/disk metrics.
Do not "fix" backpressure by weakening confirms — it is the contract working.

#### `max-length`: prefer rejection over silent trimming

If a queue is capped (via policies), set `overflow: reject-publish` in
reliable setups: the publisher gets a nack, which the at-least-once contract
surfaces as a visible error. The default `drop-head` silently deletes the
oldest messages — a silent-contract violation. A capped queue plus a DLX
(`x-overflow: reject-publish-dlx` on modern RabbitMQ) routes the rejected
messages where you can see them.

#### Heartbeat and confirm timeout

- The connection-level `heartbeat` (seconds, driver side) detects half-open
  TCP — a crashed peer that never sent FIN. Keep the default; lower values
  detect dead peers faster at the cost of more broker chatter.
- `confirm_timeout` bounds how long a publish waits for its confirm. On a
  live connection a confirm timeout is **terminal** (the outcome is unknown,
  so nothing is replayed automatically); during a recovery, a parked
  publication is retried once with a fresh deadline. Sizing: the timeout must
  comfortably exceed worst-case broker stall, and the warning signs of
  approaching it are the watermarks above — see
  [Reliability — Publisher confirms](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#publisher-confirms).

#### Prefetch (driver side) — the highest-leverage knob

Prefetch bounds how many unacked messages one consumer holds — it throttles
the worker *and* bounds the broker's acker memory, because unacked messages
sit on the node until acknowledged:

- **Fixed** — `prefetch => 32`: predictable, right for uniform job durations.
- **Adaptive** — keeps about `target_buffer_seconds` of ready work buffered,
  learning job duration (EWMA) and adjusting between `min` and `max` with
  hysteresis. Right when job durations vary, so slow jobs do not stall the
  pipeline while fast jobs starve it.

```php
'subscriptions' => [
    'critical' => [
        'queue' => 'orders.critical',
        'prefetch' => [
            'mode' => 'adaptive',
            'initial' => 64, 'min' => 1, 'max' => 256,
            'target_buffer_seconds' => 5,
        ],
    ],
],
```

A runaway prefetch under slow consumers is a broker memory hazard: the node
holds every unacked delivery. Adaptive prefetch is the default-shaped answer
when in doubt. Full option reference:
[Subscriptions](#subscriptions-escape-hatch).

#### Verify the effect, not the intention

```bash
rabbitmqctl list_queues name messages_ready messages_unacknowledged
php artisan rabbit-rs:status --format=json   # same-process pool + management counters
php artisan rabbit-rs:doctor                 # wiring and safety summary
```

`rabbit-rs:status` cross-process counters (delivered / acked / redelivered
from the management API) are the ground truth for queue depth and duplicate
signal; `redelivered` also counts crash requeues, so treat it as an
approximate duplicate signal, not an error rate.

### Recipe: Capacity planning

Use the published evidence as a starting point, then measure **your** workload
— numbers are only comparable within one workload, one configuration, and one
session ([the framing rules](https://github.com/Goopil/php-rabbit-rs/blob/main/benchmarks/README.md#reading-and-quoting-results--workload-scoped-framing-only)).

#### What the published evidence covers

**Comparative (lab workloads):** on the curated lab workloads, rabbit-rs
consumes 4–6× faster than php-amqplib in the same session, with 0 losses and
0 duplicates in every reliable-mode run. That is a workload-scoped
measurement, not a promise for your workload — quote both throughput numbers
with their configuration, never a bare multiplier.

**Stability (Round K soak** — 3-node lab, release build, archived under
`benchmarks/results/round-k-soak/`):

- Steady 30 min: 5.9 M messages pop+ack (≈3.3k/s on the lab hardware), 0 loss,
  1 duplicate, RSS plateau after warmup (envelope slope +0.6 MB/h).
- Kill 60 min: 2.9 M messages across 297 forced connection kills, **0
  missing**, 17,655 duplicates (0.6 %), publish-buffer tripwire never fired,
  RSS bounded.

Expectation setting from the kill run: **duplicates under connection churn
are contract behavior, counted, not anomalies** — 0.6 % under a kill every 12
seconds is what aggressive recovery churn looks like.

#### Sizing your deployment

1. **Measure, don't extrapolate.** Run the harness on production-like
   hardware and realistic payload/job durations:

   ```bash
   ./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh
   ./benchmarks/run-benchmarks.sh --driver=rabbit-rs --scenario=laravel-worker
   # Stability + memory evidence (steady and kill modes):
   php benchmarks/driver-bench/bin/soak.php --minutes=30 --kill-every=0
   php benchmarks/driver-bench/bin/soak.php --minutes=60 --kill-every=10
   ```

2. **Job duration is the driver.** Steady-state throughput per worker is
   ≈ 1/mean-job-duration; scale `--workers` per connection and add consumer
   capacity per queue. Adaptive prefetch (Round E) keeps
   ~`target_buffer_seconds` of ready work buffered, so a burst does not starve
   a busy subscription — tune that target, not a fixed prefetch guess.

3. **Broker capacity.** Quorum queues cost more per message (replication +
   Raft) than classic — that is the price of surviving node loss, and the
   default here for good reason. Watch `messages_unacknowledged` alongside
   queue depth: slow consumers convert prefetch into resident node memory
   (see [Broker tuning](#broker-watermarks-what-happens-when-the-node-is-stressed)).

4. **Memory expectations.** The soak envelope is the model: RSS is bounded
   after warm-up; a genuine leak must exceed the warmup peak and keep
   climbing. Keep the soak in CI (nightly, `--leak-mb-per-hour`, default
   20 MB/h) so a regression fails a run instead of a pager.

5. **Read backpressure as a capacity signal.** `BackpressureDetected` events
   and rising `backpressure_total` in `rabbit-rs:status` mean the publisher
   side is saturated: add workers or shard connections before raising
   `confirm_timeout`.

#### Red flags

- Duplicates rising on a steady run (no kills, no restarts) → investigate;
  do not normalize it.
- `publish_buffered > 0` at rest in `Pool::stats()` → publications parked
  across cycles (the re-buffer leak path the soak tripwire watches).
- RSS climbing past the warm-up peak → rerun the envelope estimator from the
  Round K evidence before blaming the workload.
