# Laravel Usage

Rabbit RS provides a standard Laravel queue driver. It integrates with `queue:work`, `queue:listen`, and the full Laravel queue ecosystem without replacing `Illuminate\Queue\Worker`.

## Publishing jobs

### Dispatch a job

```php
use App\Jobs\ProcessOrder;

// Dispatch to the default queue
ProcessOrder::dispatch($order);

// Dispatch to a specific queue
ProcessOrder::dispatch($order)->onQueue('orders.high');
```

### Dispatch with delay

```php
// Delay by 5 minutes
ProcessOrder::dispatch($order)->delay(now()->addMinutes(5));
```

Delayed jobs use the configured delay strategy (plugin or TTL fallback). See [Topology — Delay routing](topology.md#delay-routing).

### Dispatch in bulk

```php
use App\Jobs\ProcessOrder;

$jobs = [
    new ProcessOrder(1),
    new ProcessOrder(2),
    new ProcessOrder(3),
];

// Bulk dispatch — uses a single native call for all immediate jobs
ProcessOrder::dispatchBatch($jobs);
```

### Raw payloads

```php
// Publish a raw payload (not serialized by Laravel)
Queue::connection('rabbit-rs')->pushRaw($jsonPayload, 'orders.high');
```

## Consuming jobs

### Standard queue worker

```bash
php artisan queue:work --connection=rabbit-rs
```

This uses Laravel's built-in `queue:work` command. Without `--queue`, the worker consumes the connection's default queue (its `queue` key). The `--queue` option resolves a queue or profile on that connection:

```bash
# Consume the "orders.high" queue defined on the connection
php artisan queue:work --connection=rabbit-rs --queue=orders.high
```

The connection compiles to a single worker profile (named after the connection) spanning all its queues — the `queue` key plus every `subscriptions` entry. A single `pop()` call selects the next delivery from any ready subscription using the weighted-fair scheduler.

The `--queue` value is resolved in this order:

1. A queue consumed by the connection (its `queue` key or a `subscriptions` entry's `queue`) — the connection's profile is used.
2. The connection name (the profile name) — the connection's whole profile, all subscriptions included, is used.
3. Otherwise the name is treated as a plain queue: with `auto_subscribe` enabled an implicit profile is built on the fly (opt-in, see below); without it `pop()` fails with an actionable error.

### Multi-process supervisor

```bash
php artisan rabbit-rs:work --workers=4
```

The `rabbit-rs:work` command supervises `queue:work` child processes with automatic restart on crash. With no flags it fans out: **one child per rabbit-rs connection**, each consuming every queue defined on its connection (`queue` key first, then `subscriptions` queues); `--workers` spawns children per connection.

Options:

| Option | Description | Default |
|--------|-------------|---------|
| `--connection` | Comma-separated connection names | Every rabbit-rs connection |
| `--queue` | Comma-separated queue names, resolved by definition (connection `queue` key or `subscriptions` alias) | Every defined queue |
| `--workers` | Children spawned per connection | `1` |
| `--max-restarts` | Max restarts per worker | `3` |
| `--backoff` | Base backoff in seconds | `1` |

Unknown connection or queue names fail with a typed error listing what is available. A queue defined on two targeted connections is consumed on both — see [Worker fan-out](configuration.md#worker-fan-out) for the full semantics.

Each child runs `queue:work` with a unique worker name. On crash, the supervisor restarts the child with exponential backoff (capped at 60 seconds). On `SIGTERM`/`SIGINT`, the supervisor gracefully stops all children.

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | Clean shutdown |
| `1` | Max restarts exceeded |
| `130` | Signal received |

### Status command

```bash
php artisan rabbit-rs:status
```

Displays connection state, pool metrics, consumer stats, and latency histograms. For machine-readable output:

```bash
php artisan rabbit-rs:status --format=json
```

The status command is read-only. It does not reconnect or modify topology.

## RabbitMqQueue API

The `RabbitMqQueue` class implements `Illuminate\Contracts\Queue\Queue` and `Illuminate\Contracts\Queue\ClearableQueue`.

### push

```php
Queue::connection('rabbit-rs')->push(ProcessOrder::class, ['orderId' => 42], 'orders.high');
```

### pushRaw

```php
Queue::connection('rabbit-rs')->pushRaw($rawPayload, 'orders.high', ['content_type' => 'application/json']);
```

### later

```php
Queue::connection('rabbit-rs')->later(300, ProcessOrder::class, ['orderId' => 42], 'orders.high');
```

The delay is specified in seconds. Rabbit RS converts it to milliseconds and routes through the delay strategy.

### bulk

```php
$messageIds = Queue::connection('rabbit-rs')->bulk([
    new ProcessOrder(1),
    new ProcessOrder(2),
    new ProcessOrder(3),
], '', 'orders.high');
```

Bulk publishing uses a single native call (`publishBatch`) for all immediate jobs. Jobs marked `dispatchAfterCommit` are deferred to the transaction commit callback.

### pop

```php
$job = Queue::connection('rabbit-rs')->pop('orders.high');
if ($job !== null) {
    $job->fire();
}
```

`pop()` delegates to the native consumer set. The queue argument is resolved on the connection (see the resolution order above): a queue the connection consumes (`queue` key or `subscriptions`), the connection name (its whole profile), or — when `auto_subscribe` is enabled — an implicit profile dedicated to the requested queue. A single call selects the next delivery from any ready subscription using the weighted-fair scheduler.

### size

```php
$depth = Queue::connection('rabbit-rs')->size('orders.high');
```

Returns the message count for the queue. Uses AMQP passive declaration (no management API required).

### clear

```php
$purged = Queue::connection('rabbit-rs')->clear('orders.high');
```

Purges all messages from the queue and returns the number of jobs removed (the pending count measured before the purge; messages racing the purge are counted but may survive). Requires configuration permissions on the broker. The `ClearableQueue` contract makes `php artisan queue:clear rabbit-rs` available.

Purges all messages from the queue. Requires configuration permissions on the broker.

## Events

Rabbit RS dispatches two native events through the Laravel event system:

### ConnectionStateChanged

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

### BackpressureDetected

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

### Custom callbacks

You can register custom callbacks directly on the `Pool` instance to replace the default event dispatch:

```php
$pool->onConnectionState(function (string $broker, string $state, int $generation): void {
    // Custom handling
});

$pool->onBackpressure(function (string $broker, int $inFlight, int $capacity): void {
    // Custom handling
});
```

## Job class

`RabbitMqJob` extends `Illuminate\Queue\Jobs\Job` and implements:

- `getJobId()` — returns the stable `message_id` (UUID from Laravel payload)
- `getRawBody()` — returns the raw payload string
- `attempts()` — returns the delivery attempt count (from `x-acquired-count` / `x-delivery-count` headers)
- `delete()` — sends `basic.ack` and releases the delivery handle
- `release($delay)` — sends `basic.reject(requeue=true)` for delay 0, or republicates via delay strategy for delay > 0

The delivery handle is released after a terminal transition (ack, reject, or release) to prevent double-settlement.

## Laravel queue configuration

Add the connection to `config/queue.php` — one connection = one broker/vhost = one native pool:

```php
'connections' => [
    // ...
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
        'after_commit' => false,
        'auto_subscribe' => (bool) env('RABBIT_RS_AUTO_SUBSCRIBE', false),
    ],
],
```

Every other key falls back to the package defaults in `config/rabbit-rs.php` — see [Configuration](configuration.md) for the full connection reference.

Set the default connection:

```bash
QUEUE_CONNECTION=rabbit-rs
```

### Auto subscribe

`auto_subscribe` (opt-in, default `false`) controls how `pop()` resolves plain queue names the connection does not consume — for example `queue:work --queue=emails` when neither the connection's `queue` key nor its `subscriptions` escape hatch references the `emails` queue.

- `false` (default): `pop()` fails with an actionable error telling you to declare the queue on the connection (`queue` key or `subscriptions`) or enable `auto_subscribe`.
- `true`: `pop()` builds an implicit worker profile on the fly — a single subscription on this connection, weight 1, and the default prefetch. The profile is cached per queue name in process memory and reused on subsequent pops of the same queue; it is requested from the native pool by name (`__auto__.<queue>`).

The native pool resolves worker profiles from its own configuration, so auto-subscribed consumption additionally requires the native side to accept runtime-registered profiles; until then, declare the queue in the connection's `subscriptions` (or its `queue` key) for reliable consumption.

Prefer declared subscriptions in production: they control per-queue weights, prefetch, and priority classes, and they are visible to `rabbit-rs:status`. Use `auto_subscribe` for development convenience or dynamic low-traffic queues.

The value can be set per connection (`auto_subscribe` in `config/queue.php`, as above — takes precedence) or package-wide in `config/rabbit-rs.php`.

## Worker lifecycle

### Graceful shutdown

The worker handles `SIGTERM` and `SIGINT`. In-flight deliveries are not acknowledged during shutdown — RabbitMQ redelivers them after the connection closes.

### Restart signal

Send `SIGTERM` to the supervisor to restart workers gracefully. The supervisor stops all children, who finish current jobs, and then exits.

## Octane integration

See [Octane](octane.md) for Octane-specific lifecycle hooks and configuration.
