# Horizon Support Design

**Date:** 23 août 2026
**Status:** Approved

## Goal

Add Laravel Horizon support to `goopil/rabbit-rs-laravel` so that Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs. RabbitMQ remains the transport; Redis is used by Horizon for job tracking, metrics, and dashboard state.

## Background

Laravel Horizon is a dashboard and supervisor for Redis-backed queues. It tracks jobs via Redis and listens to specific events dispatched by the queue driver. The `vyuldashev/laravel-queue-rabbitmq` package demonstrates the integration pattern: a Horizon-specific subclass of the queue driver dispatches `JobPending`, `JobPushed`, `JobReserved`, and `JobDeleted` events so Horizon can track RabbitMQ jobs in Redis.

Rabbit RS currently has no Horizon integration. Jobs processed via `rabbit-rs` connections are invisible to the Horizon dashboard.

## Architecture

Two new classes in a `Horizon` namespace:

```
src/Horizon/
├── RabbitMqQueue.php    — extends RabbitMqQueue, dispatches Horizon events
└── RabbitMqJob.php      — extends RabbitMqJob, calls deleteReserved() on delete()
```

A `worker` config option selects the variant:
- `default` → `RabbitMqQueue` (current behavior, zero impact for non-Horizon users)
- `horizon` → `Horizon\RabbitMqQueue`

The connector `RabbitMqConnector::connect()` dynamically instantiates the correct class based on the `worker` value.

**No changes to Rust core or the PHP extension** — all work is in the Laravel package layer.

## Events and Job Lifecycle

Horizon uses Redis to store job state. It listens to four key events:

| Step | Horizon Event | When | Redis Action |
|------|--------------|------|-------------|
| Before push | `JobPending` | `pushRaw()` before send | Marks job as "pending" |
| After push | `JobPushed` | `pushRaw()` after send | Records job in Redis with tags, type, timestamp |
| After pop | `JobReserved` | `pop()` returns a job | Marks job as "reserved" (being processed) |
| After delete | `JobDeleted` | `delete()` on the job | Marks job as "completed/deleted" |

### `Horizon\RabbitMqQueue` overrides

- **`push($job, $data, $queue)`** — stores `$lastPushed = $job` for `JobPayload::prepare()`, then calls `parent::push()`
- **`pushRaw($payload, $queue, $options)`** — wraps the payload with `JobPayload::prepare($this->lastPushed)`, dispatches `JobPending`, calls `parent::pushRaw()`, then dispatches `JobPushed`
- **`later($delay, $job, $data, $queue)`** — wraps the payload with `JobPayload::prepare($job)`, dispatches `JobPending`, calls `parent::later()`, then dispatches `JobPushed`
- **`pop($queue)`** — calls `parent::pop()`, if a job is returned dispatches `JobReserved` with the job's raw body
- **`marshalJob($delivery, $queue)`** — overridden to create `Horizon\RabbitMqJob` instead of `RabbitMqJob`, passing `$this` (the queue instance) to the constructor
- **`deleteReserved($queue, $job)`** — dispatches `JobDeleted`

### `Horizon\RabbitMqJob` overrides

The Horizon Job stores a reference to the `HorizonRabbitMqQueue` instance (passed via `marshalJob`) so it can call `deleteReserved()`:

- **Constructor** — accepts an additional `HorizonRabbitMqQueue $queue` parameter (passed by `marshalJob`)
- **`delete()`** — calls `parent::delete()`, then calls `$this->rabbitmqQueue->deleteReserved($this->queue, $this)`
- **`release($delay)`** — calls `parent::release()` (Horizon handles the release via the re-push)

### `JobPayload::prepare()`

This Horizon utility adds `type`, `tags`, `silenced`, and `pushedAt` fields to the job payload JSON. These fields power the dashboard's filtering, tagging, and timing display.

### Event dispatch helper

Both classes use a shared `event()` helper method that checks if a `Dispatcher` is bound in the container before dispatching, following the same pattern as `vyuldashev/laravel-queue-rabbitmq`:

```php
protected function event(string $queue, mixed $event): void
{
    if ($this->container && $this->container->bound(Dispatcher::class)) {
        $this->container->make(Dispatcher::class)->dispatch(
            $event->connection($this->connectionName)->queue($queue)
        );
    }
}
```

## Config and Connector

### `config/rabbit-rs.php`

New key:

```php
'worker' => env('RABBIT_RS_WORKER', 'default'),
```

### `config/queue.php` (application side)

The Rabbit RS connection can also specify `worker`:

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'worker' => env('RABBIT_RS_WORKER', 'default'),
        // ...
    ],
],
```

### `RabbitMqConnector::connect()`

Resolves the class dynamically:

```php
$worker = $config['worker'] ?? 'default';
$class = $worker === 'horizon'
    ? HorizonRabbitMqQueue::class
    : RabbitMqQueue::class;

return new $class(/* same constructor args as today */);
```

### Coexistence with Redis queues

- `config/queue.php` contains both `redis` (for Horizon) and `rabbit-rs` connections
- `config/horizon.php` can configure supervisors for both connection types:

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

- Horizon supervisors spawn `queue:work` processes for each connection
- Rabbit RS workers can also run via `rabbit-rs:work` independently
- Both types of jobs appear in the same Horizon dashboard

## Changes to Existing Code

### `RabbitMqQueue`

- Remove `final` keyword from class declaration (currently `final class RabbitMqQueue`) — allows `Horizon\RabbitMqQueue` to extend it
- `pushRaw()` is already `public` — the Horizon subclass calls `parent::pushRaw()` which internally calls the `private publish()`. No visibility change needed on `publish()`.
- `marshalJob()` is already `public` — the Horizon subclass overrides it to create `Horizon\RabbitMqJob` with the queue reference
- `pop()` is already `public` — the Horizon subclass wraps it to dispatch `JobReserved`
- No other signature changes needed

### `RabbitMqJob`

- Remove `final` keyword from class declaration (currently `final class RabbitMqJob`) — allows `Horizon\RabbitMqJob` to extend it
- Constructor, `delete()`, `release()` remain public
- The Horizon subclass adds the queue instance reference via its own constructor (which calls `parent::__construct()`)

### `RabbitMqConnector`

- Dynamic class resolution based on `worker` config value

### No changes

- Rust core (`rabbit-rs-core`)
- PHP extension (`rabbit-rs-php`)
- `ConfigNormalizer` (the `worker` key passes through as part of the per-connection config, not part of the native config)
- `NativePoolFactory`
- `WorkerProfileResolver`
- `OctaneLifecycle`
- `WorkerSupervisor`
- Existing events (`ConnectionStateChanged`, `BackpressureDetected`)

## Testing

### Unit tests (without extension, fake classes)

**`HorizonRabbitMqQueueTest`**:
- `push()` dispatches `JobPending` then `JobPushed` with prepared payload (tags, type, pushedAt)
- `later()` dispatches `JobPending` then `JobPushed`
- `pop()` dispatches `JobReserved` when a job is returned
- `pop()` dispatches nothing when null
- `deleteReserved()` dispatches `JobDeleted`
- Payload passed to events contains Horizon fields (`type`, `tags`, `silenced`, `pushedAt`)
- Events have correct `connectionName` and `queue`
- `marshalJob()` creates `Horizon\RabbitMqJob` (not `RabbitMqJob`)
- `worker=horizon` in config → connector instantiates `Horizon\RabbitMqQueue`
- `worker=default` → connector instantiates `RabbitMqQueue` (not Horizon)

**`HorizonRabbitMqJobTest`**:
- `delete()` calls `deleteReserved()` on Horizon queue
- `delete()` after `markAsFailed()` does not call `deleteReserved()` (reject handles it)
- `release()` works correctly with re-push

### Integration test

- A job pushed on Rabbit RS, consumed, and visible in Horizon dashboard (verified via Redis state)

## Dependencies

- `laravel/horizon` is an optional dependency, required only when `worker=horizon`
- The package should suggest `laravel/horizon` in composer but not require it
- Classes in `Horizon/` namespace import from `Laravel\Horizon\*` and are only loaded when Horizon is installed

## Scope

- V1: event dispatching for dashboard visibility
- V1: coexistence with Redis queues in the same Horizon instance
- V1: Horizon supervisor management of Rabbit RS workers (start/stop/restart via `queue:work --connection=rabbit-rs`)
- Out of scope: replacing Redis backend entirely, Horizon auto-scaling for Rabbit RS (uses Horizon's built-in process management)
