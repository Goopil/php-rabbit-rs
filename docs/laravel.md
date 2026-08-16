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

This uses Laravel's built-in `queue:work` command. The `--queue` option references a worker profile name (default: `default`):

```bash
# Consume from the "main" worker profile
php artisan queue:work --connection=rabbit-rs --queue=main
```

The worker profile resolves to all configured subscriptions. A single `pop()` call selects the next delivery from any ready subscription using the weighted-fair scheduler.

### Multi-process supervisor

```bash
php artisan rabbit-rs:work --workers=4
```

The `rabbit-rs:work` command supervises multiple `queue:work` child processes with automatic restart on crash.

Options:

| Option | Description | Default |
|--------|-------------|---------|
| `--connection` | Queue connection name | `rabbit-rs` |
| `--queue` | Queue/profile name | `default` |
| `--workers` | Number of child workers | `1` |
| `--max-restarts` | Max restarts per worker | `3` |
| `--backoff` | Base backoff in seconds | `1` |

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
$job = Queue::connection('rabbit-rs')->pop('main');
if ($job !== null) {
    $job->fire();
}
```

`pop()` delegates to the native consumer set. The queue argument references a worker profile name. A single call selects the next delivery from any ready subscription using the weighted-fair scheduler.

### size

```php
$depth = Queue::connection('rabbit-rs')->size('orders.high');
```

Returns the message count for the queue. Uses AMQP passive declaration (no management API required).

### clear

```php
Queue::connection('rabbit-rs')->clear('orders.high');
```

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

Add the connection to `config/queue.php`:

```php
'connections' => [
    // ...
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'after_commit' => false,
    ],
],
```

Set the default connection:

```bash
QUEUE_CONNECTION=rabbit-rs
```

## Worker lifecycle

### Graceful shutdown

The worker handles `SIGTERM` and `SIGINT`. In-flight deliveries are not acknowledged during shutdown — RabbitMQ redelivers them after the connection closes.

### Restart signal

Send `SIGTERM` to the supervisor to restart workers gracefully. The supervisor stops all children, who finish current jobs, and then exits.

## Octane integration

See [Octane](octane.md) for Octane-specific lifecycle hooks and configuration.
